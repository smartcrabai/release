use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;
use crate::backends::workspace::child_manifests;

pub struct Cargo;

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

        if let Ok(doc) = Self::read_doc(root)
            && let Layout::VirtualOrMembers { members } = Self::classify(&doc)
            && let Ok(children) = Self::member_manifests(root, &members)
        {
            v.extend(children);
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
                let Some(name) = Self::package_name_from_doc(&doc) else {
                    return Err(anyhow!(
                        "cannot determine [package].name for workspace publish"
                    ));
                };
                super::run(root, "cargo", &["publish", "-p", &name])
            }
            Layout::Package => super::run(root, "cargo", &["publish"]),
            Layout::VirtualOrMembers { members } => {
                for rel in Self::member_manifests(root, &members)? {
                    let member_doc = Self::read_member_doc(&root.join(&rel))?;
                    let Some(name) = Self::package_name_from_doc(&member_doc) else {
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
        }
    }

    fn publish_command_preview(&self, root: &Path) -> Result<Option<String>> {
        let doc = Self::read_doc(root)?;
        match Self::classify(&doc) {
            Layout::WorkspacePackage => {
                let name = Self::package_name_from_doc(&doc).ok_or_else(|| {
                    anyhow!("cannot determine [package].name for workspace publish")
                })?;
                Ok(Some(format!("cargo publish -p {name}")))
            }
            Layout::Package => Ok(Some("cargo publish".into())),
            Layout::VirtualOrMembers { members } => {
                let mut names: Vec<String> = Vec::new();
                for rel in Self::member_manifests(root, &members)? {
                    let member_doc = Self::read_member_doc(&root.join(&rel))?;
                    if let Some(n) = Self::package_name_from_doc(&member_doc) {
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
}
