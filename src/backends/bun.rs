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

/// Update every `workspaces.<path>.version` entry in `bun.lock` to match the
/// `version` field of the matching on-disk `package.json`. Touches only the
/// workspace section; external package resolutions are left alone.
fn sync_bun_lockfile_workspace_versions(root: &Path) -> Result<()> {
    let lockfile = root.join("bun.lock");
    if !lockfile.is_file() {
        return Ok(());
    }
    let mut content = std::fs::read_to_string(&lockfile)
        .with_context(|| format!("read {}", lockfile.display()))?;

    let root_pkg = root.join("package.json");
    if root_pkg.is_file()
        && let Some(v) = parse_package_json(&root_pkg)?
            .get("version")
            .and_then(Value::as_str)
    {
        content = replace_workspace_version_in_lockfile(&content, "", v)?;
    }

    for rel in bun_child_package_jsons(root)? {
        let parent = rel.parent().unwrap_or(Path::new(""));
        let parent_str = parent.to_string_lossy().replace('\\', "/");
        if parent_str.is_empty() {
            continue;
        }
        let pkg_path = root.join(&rel);
        let Some(version) = parse_package_json(&pkg_path)?
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        content = replace_workspace_version_in_lockfile(&content, &parent_str, &version)?;
    }

    std::fs::write(&lockfile, content).with_context(|| format!("write {}", lockfile.display()))?;
    Ok(())
}

/// Replace the `"version"` field inside the `"workspaces"."<key>"` object of a
/// `bun.lock` document. Returns the input unchanged when the key is absent or
/// when the block has no `"version"` field. Errors only on malformed lockfile
/// structure (unbalanced braces, unterminated string).
fn replace_workspace_version_in_lockfile(
    content: &str,
    workspace_key: &str,
    new_version: &str,
) -> Result<String> {
    let ws_anchor = "\"workspaces\":";
    let Some(ws_idx) = content.find(ws_anchor) else {
        return Ok(content.to_owned());
    };
    let after_ws = &content[ws_idx + ws_anchor.len()..];
    let Some(ws_open_rel) = after_ws.find('{') else {
        return Ok(content.to_owned());
    };
    let ws_open_abs = ws_idx + ws_anchor.len() + ws_open_rel;
    let ws_close_abs = match_brace(content, ws_open_abs)?;

    let key_pattern = format!("\"{workspace_key}\":");
    let ws_section = &content[ws_open_abs..=ws_close_abs];
    let Some(key_rel) = ws_section.find(&key_pattern) else {
        return Ok(content.to_owned());
    };
    let key_abs = ws_open_abs + key_rel;

    let after_key = &content[key_abs + key_pattern.len()..];
    let Some(block_open_rel) = after_key.find('{') else {
        return Ok(content.to_owned());
    };
    let block_open_abs = key_abs + key_pattern.len() + block_open_rel;
    let block_close_abs = match_brace(content, block_open_abs)?;

    let block = &content[block_open_abs..=block_close_abs];
    let version_key = "\"version\":";
    let Some(version_rel) = block.find(version_key) else {
        return Ok(content.to_owned());
    };
    let after_version = &content[block_open_abs + version_rel + version_key.len()..];
    let Some(quote_rel) = after_version.find('"') else {
        return Err(anyhow!("malformed bun.lock: version value not quoted"));
    };
    let value_start = block_open_abs + version_rel + version_key.len() + quote_rel + 1;
    let Some(end_quote_rel) = content[value_start..].find('"') else {
        return Err(anyhow!("malformed bun.lock: unterminated version string"));
    };
    let value_end = value_start + end_quote_rel;

    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..value_start]);
    out.push_str(new_version);
    out.push_str(&content[value_end..]);
    Ok(out)
}

/// Given the byte index of an opening `{`, return the index of its matching
/// `}`. Tracks `"..."` strings (with `\"` escapes) so braces inside string
/// values don't perturb the depth counter.
fn match_brace(content: &str, open: usize) -> Result<usize> {
    let bytes = content.as_bytes();
    if open >= bytes.len() || bytes[open] != b'{' {
        return Err(anyhow!("internal: match_brace called on non-`{{` byte"));
    }
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err(anyhow!("malformed bun.lock: unbalanced braces"))
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
        // `bun install` does not re-sync workspace member `version` fields in
        // an existing `bun.lock`. Without this, `bun publish` resolves
        // `workspace:^`/`workspace:~` in published tarballs against stale
        // versions baked into the lockfile, producing dependency ranges that
        // lag the just-bumped release. Surgically patch the lockfile first.
        sync_bun_lockfile_workspace_versions(root)?;
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
            return Ok(());
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
            return Ok(None);
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

    fn is_publishable(&self, root: &Path) -> Result<bool> {
        Ok(!publish_dirs(root)?.is_empty())
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
    fn private_single_package_is_not_publishable() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("package.json"),
            "{ \"name\": \"solo\", \"version\": \"1.0.0\", \"private\": true }\n",
        )?;
        let backend = Bun;
        assert!(!backend.is_publishable(tmp.path())?);
        assert_eq!(backend.publish_command_preview(tmp.path())?, None);
        backend.publish(tmp.path())?;
        Ok(())
    }

    #[test]
    fn workspace_with_all_private_packages_is_not_publishable() -> Result<()> {
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
        assert!(!backend.is_publishable(root)?);
        assert_eq!(backend.publish_command_preview(root)?, None);
        backend.publish(root)?;
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
    fn replace_workspace_version_updates_member_block() -> Result<()> {
        let lock = "{\n  \"workspaces\": {\n    \"\": {\n      \"name\": \"root\"\n    },\n    \"packages/cli\": {\n      \"name\": \"@x/cli\",\n      \"version\": \"0.1.14\",\n      \"dependencies\": {\n        \"@x/sdk\": \"workspace:^\"\n      }\n    },\n    \"packages/sdk\": {\n      \"name\": \"@x/sdk\",\n      \"version\": \"0.1.14\"\n    }\n  }\n}\n";
        let updated = replace_workspace_version_in_lockfile(lock, "packages/cli", "0.1.18")?;
        assert!(updated.contains("\"packages/cli\""), "{updated}");
        let cli_block_start = updated
            .find("\"packages/cli\"")
            .ok_or_else(|| anyhow!("missing cli key"))?;
        let cli_block_end = updated[cli_block_start..]
            .find("\"packages/sdk\"")
            .map(|x| cli_block_start + x)
            .ok_or_else(|| anyhow!("missing sdk key"))?;
        let cli_block = &updated[cli_block_start..cli_block_end];
        assert!(
            cli_block.contains("\"version\": \"0.1.18\""),
            "cli block did not get bumped: {cli_block}"
        );
        // sdk block must be untouched
        let sdk_block = &updated[cli_block_end..];
        assert!(
            sdk_block.contains("\"version\": \"0.1.14\""),
            "sdk block was modified: {sdk_block}"
        );
        Ok(())
    }

    #[test]
    fn replace_workspace_version_handles_root_key() -> Result<()> {
        let lock = "{\n  \"workspaces\": {\n    \"\": {\n      \"name\": \"root\",\n      \"version\": \"0.1.14\"\n    }\n  }\n}\n";
        let updated = replace_workspace_version_in_lockfile(lock, "", "0.2.0")?;
        assert!(updated.contains("\"version\": \"0.2.0\""), "{updated}");
        Ok(())
    }

    #[test]
    fn replace_workspace_version_is_noop_when_key_missing() -> Result<()> {
        let lock = "{\n  \"workspaces\": {\n    \"packages/a\": { \"name\": \"@x/a\", \"version\": \"1.0.0\" }\n  }\n}\n";
        let updated = replace_workspace_version_in_lockfile(lock, "packages/b", "9.9.9")?;
        assert_eq!(updated, lock);
        Ok(())
    }

    #[test]
    fn replace_workspace_version_is_noop_when_block_has_no_version() -> Result<()> {
        let lock =
            "{\n  \"workspaces\": {\n    \"\": { \"name\": \"root\", \"private\": true }\n  }\n}\n";
        let updated = replace_workspace_version_in_lockfile(lock, "", "9.9.9")?;
        assert_eq!(updated, lock);
        Ok(())
    }

    #[test]
    fn match_brace_skips_braces_inside_strings() -> Result<()> {
        let s = r#"{ "k": "value with } brace", "n": 1 }"#;
        let close = match_brace(s, 0)?;
        assert_eq!(&s[close..=close], "}");
        assert_eq!(close, s.len() - 1);
        Ok(())
    }

    #[test]
    fn sync_bun_lockfile_workspace_versions_updates_members() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            "{\n  \"name\": \"root\",\n  \"version\": \"0.2.0\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
        )?;
        fs::create_dir_all(root.join("packages/cli"))?;
        fs::create_dir_all(root.join("packages/sdk"))?;
        fs::write(
            root.join("packages/cli/package.json"),
            "{ \"name\": \"@x/cli\", \"version\": \"0.2.0\", \"dependencies\": { \"@x/sdk\": \"workspace:^\" } }\n",
        )?;
        fs::write(
            root.join("packages/sdk/package.json"),
            "{ \"name\": \"@x/sdk\", \"version\": \"0.2.0\" }\n",
        )?;
        // Lockfile carries the *previous* versions (0.1.0) — what we get from a
        // stale `bun install`.
        fs::write(
            root.join("bun.lock"),
            "{\n  \"workspaces\": {\n    \"\": { \"name\": \"root\", \"version\": \"0.1.0\" },\n    \"packages/cli\": { \"name\": \"@x/cli\", \"version\": \"0.1.0\", \"dependencies\": { \"@x/sdk\": \"workspace:^\" } },\n    \"packages/sdk\": { \"name\": \"@x/sdk\", \"version\": \"0.1.0\" }\n  }\n}\n",
        )?;

        sync_bun_lockfile_workspace_versions(root)?;

        let after = fs::read_to_string(root.join("bun.lock"))?;
        // All three workspace entries should now reflect 0.2.0.
        assert_eq!(
            after.matches("\"version\": \"0.2.0\"").count(),
            3,
            "{after}"
        );
        assert!(!after.contains("\"version\": \"0.1.0\""), "{after}");
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
