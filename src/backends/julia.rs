use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;

pub struct Julia;

impl Julia {
    fn manifest_path(root: &Path) -> PathBuf {
        root.join("Project.toml")
    }

    fn read_doc(root: &Path) -> Result<DocumentMut> {
        let path = Self::manifest_path(root);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parse {}", path.display()))
    }
}

impl Backend for Julia {
    fn name(&self) -> &'static str {
        "julia"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        let doc = Self::read_doc(root)?;
        doc.get("version")
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("no top-level `version` in Project.toml"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        let path = Self::manifest_path(root);
        let mut doc = Self::read_doc(root)?;
        if !doc.contains_key("version") {
            return Err(anyhow!("no top-level `version` in Project.toml"));
        }
        doc["version"] = toml_edit::value(new);
        fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn update_lockfile(&self, _root: &Path) -> Result<()> {
        Ok(())
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        None
    }

    fn files_to_stage(&self, _root: &Path) -> Vec<PathBuf> {
        vec![PathBuf::from("Project.toml")]
    }

    fn publish(&self, _root: &Path) -> Result<()> {
        Ok(())
    }

    fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn roundtrip() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let manifest = "name = \"Demo\"\nuuid = \"12345678-1234-5678-1234-567812345678\"\nversion = \"0.5.0\"\n";
        fs::write(tmp.path().join("Project.toml"), manifest)?;
        let b = Julia;
        assert_eq!(b.read_version(tmp.path())?, "0.5.0");
        b.write_version(tmp.path(), "0.6.0")?;
        let after = fs::read_to_string(tmp.path().join("Project.toml"))?;
        assert!(after.contains("version = \"0.6.0\""));
        assert!(after.contains("name = \"Demo\""));
        Ok(())
    }
}
