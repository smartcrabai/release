use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;
use crate::backends::workspace::child_manifests;

pub struct Uv;

impl Uv {
    fn manifest_path(root: &Path) -> PathBuf {
        root.join("pyproject.toml")
    }

    fn read_doc(root: &Path) -> Result<DocumentMut> {
        let path = Self::manifest_path(root);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parse {}", path.display()))
    }

    fn workspace_members(doc: &DocumentMut) -> Vec<String> {
        let Some(members) = doc
            .get("tool")
            .and_then(Item::as_table)
            .and_then(|t| t.get("uv"))
            .and_then(Item::as_table)
            .and_then(|t| t.get("workspace"))
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

    fn member_manifests(root: &Path, members: &[String]) -> Result<Vec<PathBuf>> {
        child_manifests(root, members, "pyproject.toml")
    }

    fn write_member_version(manifest: &Path, new: &str) -> Result<()> {
        let text =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        let mut doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("parse {}", manifest.display()))?;
        let Some(project) = doc.get_mut("project").and_then(Item::as_table_mut) else {
            return Ok(());
        };
        if !project.contains_key("version") {
            return Ok(());
        }
        project["version"] = toml_edit::value(new);
        fs::write(manifest, doc.to_string())
            .with_context(|| format!("write {}", manifest.display()))?;
        Ok(())
    }
}

impl Backend for Uv {
    fn name(&self) -> &'static str {
        "uv"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        let doc = Self::read_doc(root)?;
        doc.get("project")
            .and_then(Item::as_table)
            .and_then(|t| t.get("version"))
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("no [project].version in pyproject.toml"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        let path = Self::manifest_path(root);
        let mut doc = Self::read_doc(root)?;
        let project = doc
            .get_mut("project")
            .and_then(Item::as_table_mut)
            .ok_or_else(|| anyhow!("no [project] table in pyproject.toml"))?;
        project["version"] = toml_edit::value(new);
        fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;

        let members = Self::workspace_members(&doc);
        if !members.is_empty() {
            for rel in Self::member_manifests(root, &members)? {
                Self::write_member_version(&root.join(&rel), new)
                    .with_context(|| format!("update member {}", rel.display()))?;
            }
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "uv", &["lock"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("uv lock".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("pyproject.toml")];

        match Self::read_doc(root) {
            Ok(doc) => {
                let members = Self::workspace_members(&doc);
                if !members.is_empty() {
                    match Self::member_manifests(root, &members) {
                        Ok(children) => v.extend(children),
                        Err(e) => eprintln!(
                            "warning: failed to expand uv workspace members at {}: {e}",
                            root.display()
                        ),
                    }
                }
            }
            Err(e) => eprintln!(
                "warning: failed to read pyproject.toml at {}: {e}",
                root.display()
            ),
        }

        if root.join("uv.lock").is_file() {
            v.push(PathBuf::from("uv.lock"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        super::run(root, "uv", &["publish"])
    }

    fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
        Ok(Some("uv publish".into()))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn roundtrip() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let manifest = "[project]\nname = \"demo\"\nversion = \"1.0.0\"\n";
        fs::write(tmp.path().join("pyproject.toml"), manifest)?;
        let b = Uv;
        assert_eq!(b.read_version(tmp.path())?, "1.0.0");
        b.write_version(tmp.path(), "1.1.0")?;
        let after = fs::read_to_string(tmp.path().join("pyproject.toml"))?;
        assert!(after.contains("version = \"1.1.0\""));
        assert!(after.contains("name = \"demo\""));
        Ok(())
    }

    #[test]
    fn workspace_lockstep_updates_members() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        let root_manifest = "[project]\nname = \"root\"\nversion = \"1.0.0\"\n\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n";
        fs::write(root.join("pyproject.toml"), root_manifest)?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/pyproject.toml"),
            "[project]\nname = \"a\"\nversion = \"1.0.0\"\n",
        )?;
        fs::write(
            root.join("packages/b/pyproject.toml"),
            "[project]\nname = \"b\"\nversion = \"1.0.0\"\n",
        )?;

        let b = Uv;
        b.write_version(root, "1.1.0")?;

        assert_eq!(b.read_version(root)?, "1.1.0");
        let a = fs::read_to_string(root.join("packages/a/pyproject.toml"))?;
        let bs = fs::read_to_string(root.join("packages/b/pyproject.toml"))?;
        assert!(a.contains("version = \"1.1.0\""));
        assert!(bs.contains("version = \"1.1.0\""));

        let staged = b.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("pyproject.toml")));
        assert!(staged.contains(&PathBuf::from("packages/a/pyproject.toml")));
        assert!(staged.contains(&PathBuf::from("packages/b/pyproject.toml")));
        Ok(())
    }
}
