use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item, Table};

use crate::backend::Backend;
use crate::backends::workspace::child_manifests;

pub struct Cargo;

/// Dependency-table keys whose entries can reference another workspace member.
const DEP_KEYS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A publishable workspace member, used to order publishes so that each
/// member's intra-workspace dependencies are published before it.
struct MemberPkg {
    /// `[package].name`. `None` means the member is anonymous and is skipped
    /// (with a warning) at publish time, matching the existing behavior.
    name: Option<String>,
    /// Names of other workspace members this one depends on.
    intra_deps: Vec<String>,
}

/// Collect the dependency names referenced in a single dependency table.
///
/// Handles both the shorthand form (`foo = "1"`) and the detailed form
/// (`foo = { version = "1" }` / `foo = { path = "../foo" }`). The table key is
/// normally the crate name, except when the entry renames the dependency via
/// `package = "real-name"`, in which case the real crate name is recorded so
/// it can be matched against a workspace member.
fn collect_dep_names(table: &Table, out: &mut HashSet<String>) {
    for (key, item) in table {
        let resolved = item
            .as_table_like()
            .and_then(|t| t.get("package"))
            .and_then(Item::as_str);
        out.insert(resolved.unwrap_or(key).to_owned());
    }
}

/// Gather every dependency name declared by a member manifest across the
/// regular, dev, build and `target.*` dependency tables.
fn member_dep_names(doc: &DocumentMut) -> HashSet<String> {
    let mut out = HashSet::new();
    for key in DEP_KEYS {
        if let Some(table) = doc.get(key).and_then(Item::as_table) {
            collect_dep_names(table, &mut out);
        }
    }
    // `[target.<cfg>.dependencies]` / `[target.<cfg>.build-dependencies]`.
    if let Some(targets) = doc.get("target").and_then(Item::as_table) {
        for (_, cfg) in targets {
            let Some(cfg_table) = cfg.as_table() else {
                continue;
            };
            for key in ["dependencies", "build-dependencies"] {
                if let Some(table) = cfg_table.get(key).and_then(Item::as_table) {
                    collect_dep_names(table, &mut out);
                }
            }
        }
    }
    out
}

/// Classification of a Cargo manifest used to decide how to read/write the
/// version and what to stage.
enum Layout {
    /// A single `[package]` crate (no `[workspace]` at the root).
    Package,
    /// Root has `[workspace.package].version` (central version for members
    /// using `version.workspace = true`). Root may additionally have its own
    /// `[package]`; we update the `[workspace.package]` entry only.
    WorkspacePackage,
    /// Root has `[workspace]` with `members`, but no `[workspace.package]`.
    /// Each member crate has its own `[package].version` which we update in
    /// lockstep with (an implicit) root version. The root itself may or may
    /// not have a `[package]`.
    VirtualOrMembers { members: Vec<String> },
}

/// Order members so each member's intra-workspace dependencies come before it
/// (Kahn's algorithm). Only edges to named members constrain the order;
/// dependencies on external crates or anonymous members are ignored. Returns
/// an error listing the affected members if the graph has a cycle.
fn topo_sort_members(pkgs: Vec<(PathBuf, MemberPkg)>) -> Result<Vec<(PathBuf, MemberPkg)>> {
    let n = pkgs.len();
    let name_to_idx: HashMap<&str, usize> = pkgs
        .iter()
        .enumerate()
        .filter_map(|(i, (_, p))| p.name.as_deref().map(|name| (name, i)))
        .collect();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_degree: Vec<usize> = vec![0; n];
    for (i, (_, p)) in pkgs.iter().enumerate() {
        for dep in &p.intra_deps {
            if let Some(&j) = name_to_idx.get(dep.as_str())
                && j != i
            {
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
                    .1
                    .name
                    .clone()
                    .unwrap_or_else(|| pkgs[i].0.display().to_string())
            })
            .collect();
        return Err(anyhow!(
            "workspace dependency cycle prevents publish; affected members: {}",
            affected.join(", ")
        ));
    }

    // `order` is a permutation of 0..n, so each entry is taken exactly once.
    let mut slots: Vec<Option<(PathBuf, MemberPkg)>> = pkgs.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .filter_map(|i| slots[i].take())
        .collect())
}

impl Cargo {
    fn manifest_path(root: &Path) -> PathBuf {
        root.join("Cargo.toml")
    }

    fn read_doc(root: &Path) -> Result<DocumentMut> {
        let path = Self::manifest_path(root);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parse {}", path.display()))
    }

    fn workspace_members(doc: &DocumentMut) -> Vec<String> {
        let Some(members) = doc
            .get("workspace")
            .and_then(Item::as_table)
            .and_then(|t| t.get("members"))
            .and_then(Item::as_array)
        else {
            return Vec::new();
        };
        members
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    fn has_workspace_package_version(doc: &DocumentMut) -> bool {
        doc.get("workspace")
            .and_then(Item::as_table)
            .and_then(|t| t.get("package"))
            .and_then(Item::as_table)
            .is_some_and(|t| t.contains_key("version"))
    }

    fn classify(doc: &DocumentMut) -> Layout {
        if Self::has_workspace_package_version(doc) {
            return Layout::WorkspacePackage;
        }
        let has_workspace = doc.get("workspace").is_some();
        if has_workspace {
            return Layout::VirtualOrMembers {
                members: Self::workspace_members(doc),
            };
        }
        Layout::Package
    }

    fn package_name_from_doc(doc: &DocumentMut) -> Option<String> {
        doc.get("package")
            .and_then(Item::as_table)
            .and_then(|t| t.get("name"))
            .and_then(|i| i.as_str())
            .map(str::to_owned)
    }

    fn read_member_doc(manifest: &Path) -> Result<DocumentMut> {
        let text =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parse {}", manifest.display()))
    }

    fn read_package_version(manifest: &Path) -> Result<Option<String>> {
        Ok(Self::read_member_doc(manifest)?
            .get("package")
            .and_then(Item::as_table)
            .and_then(|t| t.get("version"))
            .and_then(|i| i.as_str())
            .map(str::to_owned))
    }

    fn write_package_version(manifest: &Path, new: &str) -> Result<()> {
        let text =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("parse {}", manifest.display()))?;
        let Some(pkg) = doc.get_mut("package").and_then(Item::as_table_mut) else {
            // Member without `[package]`? Skip silently.
            return Ok(());
        };
        if !pkg.contains_key("version") {
            return Ok(());
        }
        pkg["version"] = toml_edit::value(new);
        fs::write(manifest, doc.to_string())
            .with_context(|| format!("write {}", manifest.display()))?;
        Ok(())
    }

    /// Return member `Cargo.toml` paths (relative to `root`). Patterns can be
    /// either literal directories (`"crates/a"`) or globs (`"crates/*"`);
    /// `glob::glob` handles both shapes transparently.
    fn member_manifests(root: &Path, members: &[String]) -> Result<Vec<PathBuf>> {
        child_manifests(root, members, "Cargo.toml")
    }

    /// Return publishable member crate names ordered so that each crate's
    /// intra-workspace dependencies precede it.
    ///
    /// Members without a `[package].name` are anonymous: they keep their
    /// relative position in the manifest order and are reported via `rel` so
    /// callers can warn/skip, but nothing in the workspace can depend on them.
    /// Errors if the intra-workspace dependency graph contains a cycle.
    fn ordered_member_names(root: &Path, members: &[String]) -> Result<Vec<(PathBuf, MemberPkg)>> {
        let manifests = Self::member_manifests(root, members)?;
        let mut pkgs: Vec<(PathBuf, MemberPkg)> = Vec::with_capacity(manifests.len());
        for rel in manifests {
            let doc = Self::read_member_doc(&root.join(&rel))?;
            let name = Self::package_name_from_doc(&doc);
            let deps = member_dep_names(&doc);
            pkgs.push((
                rel,
                MemberPkg {
                    name,
                    intra_deps: deps.into_iter().collect(),
                },
            ));
        }
        topo_sort_members(pkgs)
    }

    fn publish_each_member(root: &Path, members: &[String]) -> Result<()> {
        for (rel, pkg) in Self::ordered_member_names(root, members)? {
            let Some(name) = pkg.name else {
                eprintln!(
                    "warning: skipping publish of {} (no [package].name)",
                    rel.display()
                );
                continue;
            };
            super::run(root, "cargo", &["publish", "-p", &name])?;
        }
        Ok(())
    }

    fn member_publish_preview(root: &Path, members: &[String]) -> Result<Option<String>> {
        let mut names: Vec<String> = Vec::new();
        for (_, pkg) in Self::ordered_member_names(root, members)? {
            if let Some(n) = pkg.name {
                names.push(n);
            }
        }
        if names.is_empty() {
            Ok(Some("cargo publish".into()))
        } else {
            let joined = names
                .iter()
                .map(|n| format!("cargo publish -p {n}"))
                .collect::<Vec<_>>()
                .join(" && ");
            Ok(Some(joined))
        }
    }
}

impl Backend for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        let doc = Self::read_doc(root)?;
        match Self::classify(&doc) {
            Layout::WorkspacePackage => doc
                .get("workspace")
                .and_then(Item::as_table)
                .and_then(|t| t.get("package"))
                .and_then(Item::as_table)
                .and_then(|t| t.get("version"))
                .and_then(|i| i.as_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("no [workspace.package].version in Cargo.toml")),
            Layout::Package => doc
                .get("package")
                .and_then(Item::as_table)
                .and_then(|t| t.get("version"))
                .and_then(|i| i.as_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("no [package].version in Cargo.toml")),
            Layout::VirtualOrMembers { members } => {
                // Prefer the root `[package].version` if present (a workspace
                // root that is also a crate).
                if let Some(v) = doc
                    .get("package")
                    .and_then(Item::as_table)
                    .and_then(|t| t.get("version"))
                    .and_then(|i| i.as_str())
                {
                    return Ok(v.to_owned());
                }
                // Virtual workspace: pick the first member with a version.
                for rel in Self::member_manifests(root, &members)? {
                    if let Some(v) = Self::read_package_version(&root.join(&rel))? {
                        return Ok(v);
                    }
                }
                Err(anyhow!(
                    "no [package].version found in workspace root or any member"
                ))
            }
        }
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        let path = Self::manifest_path(root);
        let mut doc = Self::read_doc(root)?;

        match Self::classify(&doc) {
            Layout::WorkspacePackage => {
                if let Some(ws) = doc
                    .get_mut("workspace")
                    .and_then(Item::as_table_mut)
                    .and_then(|t| t.get_mut("package"))
                    .and_then(Item::as_table_mut)
                {
                    ws["version"] = toml_edit::value(new);
                }
                fs::write(&path, doc.to_string())
                    .with_context(|| format!("write {}", path.display()))?;
            }
            Layout::Package => {
                let pkg = doc
                    .get_mut("package")
                    .and_then(Item::as_table_mut)
                    .ok_or_else(|| anyhow!("[package] missing in Cargo.toml"))?;
                pkg["version"] = toml_edit::value(new);
                fs::write(&path, doc.to_string())
                    .with_context(|| format!("write {}", path.display()))?;
            }
            Layout::VirtualOrMembers { members } => {
                // Virtual workspaces have no root `[package]`; in that case
                // leave the root manifest untouched.
                if let Some(pkg) = doc.get_mut("package").and_then(Item::as_table_mut)
                    && pkg.contains_key("version")
                {
                    pkg["version"] = toml_edit::value(new);
                    fs::write(&path, doc.to_string())
                        .with_context(|| format!("write {}", path.display()))?;
                }
                for rel in Self::member_manifests(root, &members)? {
                    Self::write_package_version(&root.join(&rel), new)
                        .with_context(|| format!("update member {}", rel.display()))?;
                }
            }
        }

        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "cargo", &["generate-lockfile"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("cargo generate-lockfile".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = vec![PathBuf::from("Cargo.toml")];

        match Self::read_doc(root) {
            Ok(doc) => {
                if let Layout::VirtualOrMembers { members } = Self::classify(&doc) {
                    match Self::member_manifests(root, &members) {
                        Ok(children) => v.extend(children),
                        Err(e) => eprintln!(
                            "warning: failed to expand cargo workspace members at {}: {e}",
                            root.display()
                        ),
                    }
                }
            }
            Err(e) => eprintln!(
                "warning: failed to read Cargo.toml at {}: {e}",
                root.display()
            ),
        }

        if root.join("Cargo.lock").is_file() {
            v.push(PathBuf::from("Cargo.lock"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        let doc = Self::read_doc(root)?;
        match Self::classify(&doc) {
            Layout::WorkspacePackage => {
                if let Some(name) = Self::package_name_from_doc(&doc) {
                    super::run(root, "cargo", &["publish", "-p", &name])
                } else {
                    Self::publish_each_member(root, &Self::workspace_members(&doc))
                }
            }
            Layout::Package => super::run(root, "cargo", &["publish"]),
            Layout::VirtualOrMembers { members } => Self::publish_each_member(root, &members),
        }
    }

    fn publish_command_preview(&self, root: &Path) -> Result<Option<String>> {
        let doc = Self::read_doc(root)?;
        match Self::classify(&doc) {
            Layout::WorkspacePackage => {
                if let Some(name) = Self::package_name_from_doc(&doc) {
                    Ok(Some(format!("cargo publish -p {name}")))
                } else {
                    Self::member_publish_preview(root, &Self::workspace_members(&doc))
                }
            }
            Layout::Package => Ok(Some("cargo publish".into())),
            Layout::VirtualOrMembers { members } => Self::member_publish_preview(root, &members),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    #[test]
    fn roundtrip_plain_package() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let manifest = "[package]\nname = \"demo\"\nversion = \"0.1.2\"\nedition = \"2021\"\n";
        fs::write(tmp.path().join("Cargo.toml"), manifest)?;
        let b = Cargo;
        assert_eq!(b.read_version(tmp.path())?, "0.1.2");
        b.write_version(tmp.path(), "0.1.3")?;
        let after = fs::read_to_string(tmp.path().join("Cargo.toml"))?;
        assert!(after.contains("version = \"0.1.3\""));
        assert!(after.contains("name = \"demo\""));
        assert_eq!(b.read_version(tmp.path())?, "0.1.3");
        Ok(())
    }

    #[test]
    fn roundtrip_workspace_package() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let manifest =
            "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"2.0.0\"\n";
        fs::write(tmp.path().join("Cargo.toml"), manifest)?;
        let b = Cargo;
        assert_eq!(b.read_version(tmp.path())?, "2.0.0");
        b.write_version(tmp.path(), "2.0.1")?;
        let after = fs::read_to_string(tmp.path().join("Cargo.toml"))?;
        assert!(after.contains("version = \"2.0.1\""));
        assert!(after.contains("[workspace.package]"));
        Ok(())
    }

    #[test]
    fn workspace_package_does_not_touch_members() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"1.0.0\"\n",
        )?;
        fs::create_dir_all(root.join("crates/a"))?;
        let child = "[package]\nname = \"a\"\nversion.workspace = true\n";
        fs::write(root.join("crates/a/Cargo.toml"), child)?;

        let b = Cargo;
        b.write_version(root, "1.0.1")?;
        let after_child = fs::read_to_string(root.join("crates/a/Cargo.toml"))?;
        assert!(after_child.contains("version.workspace = true"));
        Ok(())
    }

    #[test]
    fn virtual_workspace_lockstep_updates_members() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/a"))?;
        fs::create_dir_all(root.join("crates/b"))?;
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;

        let b = Cargo;
        assert_eq!(b.read_version(root)?, "0.1.0");
        b.write_version(root, "0.2.0")?;

        let a_after = fs::read_to_string(root.join("crates/a/Cargo.toml"))?;
        let b_after = fs::read_to_string(root.join("crates/b/Cargo.toml"))?;
        assert!(a_after.contains("version = \"0.2.0\""));
        assert!(b_after.contains("version = \"0.2.0\""));

        let staged = b.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("Cargo.toml")));
        assert!(staged.contains(&PathBuf::from("crates/a/Cargo.toml")));
        assert!(staged.contains(&PathBuf::from("crates/b/Cargo.toml")));
        Ok(())
    }

    #[test]
    fn root_package_plus_members_updates_both() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[package]\nname = \"root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::create_dir_all(root.join("crates/a"))?;
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        )?;

        let b = Cargo;
        b.write_version(root, "0.1.1")?;

        let root_after = fs::read_to_string(root.join("Cargo.toml"))?;
        let a_after = fs::read_to_string(root.join("crates/a/Cargo.toml"))?;
        assert!(root_after.contains("version = \"0.1.1\""));
        assert!(a_after.contains("version = \"0.1.1\""));
        Ok(())
    }

    #[test]
    fn publish_preview_orders_dependency_first() -> Result<()> {
        // cli depends on sdk, so sdk must be published before cli even though
        // `members` lists cli first.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/cli\", \"crates/sdk\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/cli"))?;
        fs::create_dir_all(root.join("crates/sdk"))?;
        fs::write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n\n[dependencies]\nsdk = { path = \"../sdk\" }\n",
        )?;
        fs::write(
            root.join("crates/sdk/Cargo.toml"),
            "[package]\nname = \"sdk\"\nversion = \"0.1.0\"\n",
        )?;

        let backend = Cargo;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let sdk_pos = preview
            .find("cargo publish -p sdk")
            .ok_or_else(|| anyhow!("sdk not in preview {preview}"))?;
        let cli_pos = preview
            .find("cargo publish -p cli")
            .ok_or_else(|| anyhow!("cli not in preview {preview}"))?;
        assert!(sdk_pos < cli_pos, "expected sdk before cli in {preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_orders_via_dev_dependencies() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/cli\", \"crates/sdk\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/cli"))?;
        fs::create_dir_all(root.join("crates/sdk"))?;
        fs::write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\nsdk = { path = \"../sdk\" }\n",
        )?;
        fs::write(
            root.join("crates/sdk/Cargo.toml"),
            "[package]\nname = \"sdk\"\nversion = \"0.1.0\"\n",
        )?;

        let backend = Cargo;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let sdk_pos = preview
            .find("cargo publish -p sdk")
            .ok_or_else(|| anyhow!("sdk not in preview {preview}"))?;
        let cli_pos = preview
            .find("cargo publish -p cli")
            .ok_or_else(|| anyhow!("cli not in preview {preview}"))?;
        assert!(sdk_pos < cli_pos, "expected sdk before cli in {preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_ignores_external_deps() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/a"))?;
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
        )?;

        let backend = Cargo;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        assert_eq!(preview, "cargo publish -p a", "{preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_errors_on_cycle() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/a"))?;
        fs::create_dir_all(root.join("crates/b"))?;
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        )?;
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        )?;

        let backend = Cargo;
        let err = match backend.publish_command_preview(root) {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("expected cycle error"),
        };
        assert!(err.contains("cycle"), "{err}");
        let affected = err
            .rsplit("affected members: ")
            .next()
            .unwrap_or_default();
        assert!(affected.contains('a'), "{err}");
        assert!(affected.contains('b'), "{err}");
        match backend.publish(root) {
            Err(e) => assert!(format!("{e}").contains("cycle"), "{e}"),
            Ok(()) => panic!("expected publish to error on cycle"),
        }
        Ok(())
    }

    #[test]
    fn publish_preview_orders_via_renamed_dependency() -> Result<()> {
        // cli renames the sdk crate via `package = "sdk"`; the edge must still
        // be detected so sdk is published before cli.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/cli\", \"crates/sdk\"]\nresolver = \"2\"\n",
        )?;
        fs::create_dir_all(root.join("crates/cli"))?;
        fs::create_dir_all(root.join("crates/sdk"))?;
        fs::write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n\n[dependencies]\nsdk_alias = { package = \"sdk\", path = \"../sdk\" }\n",
        )?;
        fs::write(
            root.join("crates/sdk/Cargo.toml"),
            "[package]\nname = \"sdk\"\nversion = \"0.1.0\"\n",
        )?;

        let backend = Cargo;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let sdk_pos = preview
            .find("cargo publish -p sdk")
            .ok_or_else(|| anyhow!("sdk not in preview {preview}"))?;
        let cli_pos = preview
            .find("cargo publish -p cli")
            .ok_or_else(|| anyhow!("cli not in preview {preview}"))?;
        assert!(sdk_pos < cli_pos, "expected sdk before cli in {preview}");
        Ok(())
    }

    #[test]
    fn publish_preview_chains_three_members() -> Result<()> {
        // c -> b -> a. Expected order: a, b, c.
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/c\", \"crates/b\", \"crates/a\"]\nresolver = \"2\"\n",
        )?;
        for m in ["a", "b", "c"] {
            fs::create_dir_all(root.join(format!("crates/{m}")))?;
        }
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        )?;
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        )?;
        fs::write(
            root.join("crates/c/Cargo.toml"),
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        )?;

        let backend = Cargo;
        let preview = backend.publish_command_preview(root)?.unwrap_or_default();
        let a = preview
            .find("publish -p a")
            .ok_or_else(|| anyhow!("a not in {preview}"))?;
        let b = preview
            .find("publish -p b")
            .ok_or_else(|| anyhow!("b not in {preview}"))?;
        let c = preview
            .find("publish -p c")
            .ok_or_else(|| anyhow!("c not in {preview}"))?;
        assert!(a < b && b < c, "expected a < b < c in {preview}");
        Ok(())
    }
}
