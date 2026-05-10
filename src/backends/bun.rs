use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::backend::Backend;
use crate::backends::pnpm::{
    files_to_stage_package_jsons, is_publishable_json, parse_package_json,
    read_version_with_workspace_fallback, write_package_json_version_if_present,
};
use crate::backends::workspace::child_package_jsons;

pub struct Bun;

/// Extract the workspace glob patterns from a parsed `package.json`.
///
/// Accepts both the string-array form (`"workspaces": ["packages/*"]`) and
/// the object form (`"workspaces": { "packages": ["packages/*"] }`).
fn extract_workspace_patterns(json: &Value) -> Vec<String> {
    let Some(ws) = json.get("workspaces") else {
        return Vec::new();
    };

    ws.as_array()
        .or_else(|| ws.get("packages").and_then(Value::as_array))
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn bun_child_package_jsons(root: &Path) -> Result<Vec<PathBuf>> {
    let pkg_path = root.join("package.json");
    if !pkg_path.is_file() {
        return Ok(Vec::new());
    }
    let patterns = extract_workspace_patterns(&parse_package_json(&pkg_path)?);
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    child_package_jsons(root, &patterns)
}

const NO_PUBLISHABLE_PACKAGES: &str =
    "no publishable packages: every package.json is private or missing a version";

const DEP_KEYS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

struct PublishablePkg {
    /// An empty `PathBuf` represents the workspace root.
    dir: PathBuf,
    /// `None` means the package is anonymous: it can depend on others but
    /// nothing in the workspace can depend on it.
    name: Option<String>,
    intra_deps: Vec<String>,
}

fn dep_names(json: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    for key in DEP_KEYS {
        if let Some(obj) = json.get(key).and_then(Value::as_object) {
            for k in obj.keys() {
                out.insert(k.clone());
            }
        }
    }
    out
}

fn collect_publishable_packages(root: &Path) -> Result<Vec<PublishablePkg>> {
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    let root_pkg = root.join("package.json");
    if root_pkg.is_file() {
        candidates.push((PathBuf::new(), root_pkg));
    }
    for rel in bun_child_package_jsons(root)? {
        let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
        candidates.push((parent, root.join(&rel)));
    }

    let mut parsed: Vec<(PathBuf, Value)> = Vec::new();
    for (dir, manifest) in candidates {
        let json = parse_package_json(&manifest)?;
        if is_publishable_json(&json) {
            parsed.push((dir, json));
        }
    }

    let workspace_names: HashSet<String> = parsed
        .iter()
        .filter_map(|(_, j)| j.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();

    Ok(parsed
        .into_iter()
        .map(|(dir, json)| {
            let name = json.get("name").and_then(Value::as_str).map(str::to_owned);
            let intra_deps: Vec<String> = dep_names(&json)
                .into_iter()
                .filter(|d| workspace_names.contains(d) && name.as_deref() != Some(d.as_str()))
                .collect();
            PublishablePkg {
                dir,
                name,
                intra_deps,
            }
        })
        .collect())
}

/// Order publishable packages so each package's intra-workspace dependencies
/// come before it. Returns an error listing the affected packages if the
/// dependency graph has a cycle.
fn topo_sort_dirs(pkgs: Vec<PublishablePkg>) -> Result<Vec<PathBuf>> {
    let n = pkgs.len();
    let name_to_idx: HashMap<&str, usize> = pkgs
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.name.as_deref().map(|name| (name, i)))
        .collect();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_degree: Vec<usize> = vec![0; n];
    for (i, p) in pkgs.iter().enumerate() {
        for dep in &p.intra_deps {
            if let Some(&j) = name_to_idx.get(dep.as_str()) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order: Vec<usize> = Vec::with_capacity(n);
    while let Some(u) = queue.pop_front() {
        order.push(u);
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if order.len() != n {
        let affected: Vec<String> = in_degree
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d > 0)
            .map(|(i, _)| {
                pkgs[i]
                    .name
                    .clone()
                    .unwrap_or_else(|| pkgs[i].dir.display().to_string())
            })
            .collect();
        return Err(anyhow!(
            "workspace dependency cycle prevents publish; affected packages: {}",
            affected.join(", ")
        ));
    }

    // `order` is a permutation of 0..n, so each `dir` is taken exactly once.
    let mut pkgs = pkgs;
    Ok(order
        .into_iter()
        .map(|i| std::mem::take(&mut pkgs[i].dir))
        .collect())
}

fn publish_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    topo_sort_dirs(collect_publishable_packages(root)?)
}

impl Backend for Bun {
    fn name(&self) -> &'static str {
        "bun"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        read_version_with_workspace_fallback(root, bun_child_package_jsons)
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        write_package_json_version_if_present(&root.join("package.json"), new)?;
        for rel in bun_child_package_jsons(root)? {
            write_package_json_version_if_present(&root.join(&rel), new)
                .with_context(|| format!("update child manifest {}", rel.display()))?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "bun", &["install"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("bun install".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = files_to_stage_package_jsons(root, bun_child_package_jsons, "bun");
        if root.join("bun.lock").is_file() {
            v.push(PathBuf::from("bun.lock"));
        }
        if root.join("bun.lockb").is_file() {
            v.push(PathBuf::from("bun.lockb"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        let dirs = publish_dirs(root)?;
        if dirs.is_empty() {
            return Err(anyhow!(
                "{NO_PUBLISHABLE_PACKAGES} (under {})",
                root.display()
            ));
        }
        super::ensure_npm_login(root, "bun", &["pm", "whoami"])?;
        for d in dirs {
            super::run(&root.join(&d), "bun", &["publish"])?;
        }
        Ok(())
    }

    fn publish_command_preview(&self, root: &Path) -> Result<Option<String>> {
        let dirs = publish_dirs(root)?;
        if dirs.is_empty() {
            return Ok(Some(format!("({NO_PUBLISHABLE_PACKAGES})")));
        }
        if dirs.len() == 1 && dirs[0].as_os_str().is_empty() {
            return Ok(Some("bun publish".into()));
        }
        let parts: Vec<String> = dirs
            .iter()
            .map(|d| {
                let shown = if d.as_os_str().is_empty() {
                    ".".into()
                } else {
                    d.display().to_string()
                };
                format!("(cd {shown} && bun publish)")
            })
            .collect();
        Ok(Some(parts.join(" && ")))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;
    use crate::backends::pnpm::read_package_json_version;

    #[test]
    fn roundtrip() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("package.json"),
            "{\n  \"name\": \"demo\",\n  \"version\": \"0.0.9\"\n}\n",
        )?;
        let b = Bun;
        assert_eq!(b.read_version(tmp.path())?, "0.0.9");
        b.write_version(tmp.path(), "0.0.10")?;
        assert_eq!(b.read_version(tmp.path())?, "0.0.10");
        Ok(())
    }

    #[test]
    fn workspace_write_updates_children_string_array() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();

        fs::write(
            root.join("package.json"),
            "{\n  \"name\": \"root\",\n  \"version\": \"0.5.0\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.5.0\" }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.5.0\" }\n",
        )?;

        let backend = Bun;
        backend.write_version(root, "0.6.0")?;
        assert_eq!(backend.read_version(root)?, "0.6.0");
        assert_eq!(
            read_package_json_version(&root.join("packages/a/package.json"))?,
            "0.6.0"
        );
        assert_eq!(
            read_package_json_version(&root.join("packages/b/package.json"))?,
            "0.6.0"
        );

        let staged = backend.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("packages/a/package.json")));
        assert!(staged.contains(&PathBuf::from("packages/b/package.json")));
        Ok(())
    }

    #[test]
    fn workspace_write_updates_children_object_form() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();

        fs::write(
            root.join("package.json"),
            "{\n  \"name\": \"root\",\n  \"version\": \"0.5.0\",\n  \"private\": true,\n  \"workspaces\": { \"packages\": [\"packages/*\"] }\n}\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.5.0\" }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.5.0\" }\n",
        )?;

        let backend = Bun;
        backend.write_version(root, "0.7.0")?;
        assert_eq!(
            read_package_json_version(&root.join("packages/a/package.json"))?,
            "0.7.0"
        );
        assert_eq!(
            read_package_json_version(&root.join("packages/b/package.json"))?,
            "0.7.0"
        );
        Ok(())
    }

    #[test]
    fn workspace_root_without_version_reads_from_child() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();

        fs::write(
            root.join("package.json"),
            "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.4.0\" }\n",
        )?;

        let backend = Bun;
        assert_eq!(backend.read_version(root)?, "0.4.0");
        backend.write_version(root, "0.4.1")?;
        let root_after = fs::read_to_string(root.join("package.json"))?;
        assert!(!root_after.contains("\"version\""));
        assert_eq!(
            read_package_json_version(&root.join("packages/a/package.json"))?,
            "0.4.1"
        );

        let staged = backend.files_to_stage(root);
        assert!(!staged.contains(&PathBuf::from("package.json")));
        assert!(staged.contains(&PathBuf::from("packages/a/package.json")));
        Ok(())
    }

    #[test]
    fn publish_preview_single_package() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("package.json"),
            "{ \"name\": \"solo\", \"version\": \"1.0.0\" }\n",
        )?;
        let backend = Bun;
        assert_eq!(
            backend.publish_command_preview(tmp.path())?,
            Some("bun publish".into())
        );
        Ok(())
    }

    #[test]
    fn publish_preview_workspace_skips_private_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\" }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\" }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(
            preview.contains("(cd packages/a && bun publish)"),
            "{preview}"
        );
        assert!(
            preview.contains("(cd packages/b && bun publish)"),
            "{preview}"
        );
        assert!(!preview.contains("(cd . &&"), "{preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_workspace_includes_publishable_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"version\": \"1.0.0\", \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"1.0.0\" }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(preview.contains("(cd . && bun publish)"), "{preview}");
        assert!(
            preview.contains("(cd packages/a && bun publish)"),
            "{preview}"
        );
        Ok(())
    }

    #[test]
    fn publish_preview_workspace_skips_private_child() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/pub"))?;
        fs::create_dir_all(root.join("packages/priv"))?;
        fs::write(
            root.join("packages/pub/package.json"),
            "{ \"name\": \"@x/pub\", \"version\": \"0.1.0\" }\n",
        )?;
        fs::write(
            root.join("packages/priv/package.json"),
            "{ \"name\": \"@x/priv\", \"version\": \"0.1.0\", \"private\": true }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(
            preview.contains("(cd packages/pub && bun publish)"),
            "{preview}"
        );
        assert!(!preview.contains("packages/priv"), "{preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_single_private_package_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("package.json"),
            "{ \"name\": \"solo\", \"version\": \"1.0.0\", \"private\": true }\n",
        )?;
        let backend = Bun;
        let preview = backend
            .publish_command_preview(tmp.path())?
            .unwrap_or_default();
        assert!(preview.contains("no publishable packages"), "{preview}");
        match backend.publish(tmp.path()) {
            Err(e) => assert!(
                format!("{e}").contains("no publishable packages"),
                "got {e}"
            ),
            Ok(()) => panic!("expected publish to error for private root package"),
        }
        Ok(())
    }

    #[test]
    fn publish_preview_workspace_no_publishable_packages() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"private\": true }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(preview.contains("no publishable packages"), "{preview}");
        match backend.publish(root) {
            Err(e) => assert!(
                format!("{e}").contains("no publishable packages"),
                "got {e}"
            ),
            Ok(()) => panic!("expected publish to error when no packages are publishable"),
        }
        Ok(())
    }

    #[test]
    fn publish_topo_orders_dependencies_first() -> Result<()> {
        // packages/a depends on packages/b, so b must run before a.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/b\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\" }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let b_pos = preview
            .find("packages/b")
            .ok_or_else(|| anyhow!("b not in preview {preview}"))?;
        let a_pos = preview
            .find("packages/a")
            .ok_or_else(|| anyhow!("a not in preview {preview}"))?;
        assert!(b_pos < a_pos, "expected b before a in {preview}");
        Ok(())
    }

    #[test]
    fn publish_topo_orders_via_dev_dependencies() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"devDependencies\": { \"@x/b\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\" }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let b_pos = preview
            .find("packages/b")
            .ok_or_else(|| anyhow!("b not in preview {preview}"))?;
        let a_pos = preview
            .find("packages/a")
            .ok_or_else(|| anyhow!("a not in preview {preview}"))?;
        assert!(b_pos < a_pos, "expected b before a in {preview}");
        Ok(())
    }

    #[test]
    fn publish_topo_ignores_external_deps() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"dependencies\": { \"react\": \"^18\" } }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(
            preview.contains("(cd packages/a && bun publish)"),
            "{preview}"
        );
        Ok(())
    }

    #[test]
    fn publish_topo_skips_private_dep_target() -> Result<()> {
        // A private workspace package isn't publishable, so a dep edge to it
        // shouldn't constrain ordering.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/priv"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/priv\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/priv/package.json"),
            "{ \"name\": \"@x/priv\", \"version\": \"0.1.0\", \"private\": true }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert!(
            preview.contains("(cd packages/a && bun publish)"),
            "{preview}"
        );
        assert!(!preview.contains("packages/priv"), "{preview}");
        Ok(())
    }

    #[test]
    fn publish_topo_errors_on_cycle() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/b\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/a\": \"workspace:*\" } }\n",
        )?;

        let backend = Bun;
        let err = match backend.publish_command_preview(root) {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("expected cycle error"),
        };
        assert!(err.contains("cycle"), "{err}");
        assert!(err.contains("@x/a"), "{err}");
        assert!(err.contains("@x/b"), "{err}");
        match backend.publish(root) {
            Err(e) => assert!(format!("{e}").contains("cycle"), "{e}"),
            Ok(()) => panic!("expected publish to error on cycle"),
        }
        Ok(())
    }

    #[test]
    fn publish_topo_dedupes_dep_across_keys() -> Result<()> {
        // The same workspace dep listed in both dependencies and
        // devDependencies must not double-count an edge.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/b\": \"workspace:*\" }, \"devDependencies\": { \"@x/b\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\" }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let b_pos = preview
            .find("packages/b")
            .ok_or_else(|| anyhow!("b not in {preview}"))?;
        let a_pos = preview
            .find("packages/a")
            .ok_or_else(|| anyhow!("a not in {preview}"))?;
        assert!(b_pos < a_pos, "expected b before a in {preview}");
        Ok(())
    }

    #[test]
    fn publish_topo_chains_three_packages() -> Result<()> {
        // c → b → a (c depends on b, b depends on a). Expected order: a, b, c.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{ \"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"] }\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::create_dir_all(root.join("packages/c"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"0.1.0\" }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/a\": \"workspace:*\" } }\n",
        )?;
        fs::write(
            root.join("packages/c/package.json"),
            "{ \"name\": \"@x/c\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/b\": \"workspace:*\" } }\n",
        )?;

        let backend = Bun;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let a = preview
            .find("packages/a")
            .ok_or_else(|| anyhow!("a not in {preview}"))?;
        let b = preview
            .find("packages/b")
            .ok_or_else(|| anyhow!("b not in {preview}"))?;
        let c = preview
            .find("packages/c")
            .ok_or_else(|| anyhow!("c not in {preview}"))?;
        assert!(a < b && b < c, "expected a < b < c in {preview}");
        Ok(())
    }

    #[test]
    fn extract_patterns_handles_both_forms() -> Result<()> {
        let arr: Value = serde_json::from_str(r#"{"workspaces":["packages/*","apps/*"]}"#)?;
        assert_eq!(
            extract_workspace_patterns(&arr),
            vec!["packages/*".to_owned(), "apps/*".to_owned()]
        );
        let obj: Value = serde_json::from_str(r#"{"workspaces":{"packages":["packages/*"]}}"#)?;
        assert_eq!(
            extract_workspace_patterns(&obj),
            vec!["packages/*".to_owned()]
        );
        let none: Value = serde_json::from_str(r#"{"name":"x"}"#)?;
        assert!(extract_workspace_patterns(&none).is_empty());
        Ok(())
    }
}
