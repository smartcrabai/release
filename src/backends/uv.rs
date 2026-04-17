use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;

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
}
