use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

use crate::backend::Backend;

pub struct Cargo;

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

    fn is_workspace(doc: &DocumentMut) -> bool {
        doc.get("workspace").is_some()
    }

    fn workspace_package_name(doc: &DocumentMut) -> Option<String> {
        doc.get("package")
            .and_then(Item::as_table)
            .and_then(|t| t.get("name"))
            .and_then(|i| i.as_str())
            .map(str::to_owned)
    }
}

impl Backend for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        let doc = Self::read_doc(root)?;
        if let Some(v) = doc
            .get("workspace")
            .and_then(Item::as_table)
            .and_then(|t| t.get("package"))
            .and_then(Item::as_table)
            .and_then(|t| t.get("version"))
            .and_then(|i| i.as_str())
        {
            return Ok(v.to_owned());
        }
        if let Some(v) = doc
            .get("package")
            .and_then(Item::as_table)
            .and_then(|t| t.get("version"))
            .and_then(|i| i.as_str())
        {
            return Ok(v.to_owned());
        }
        Err(anyhow!(
            "no [package].version or [workspace.package].version in Cargo.toml"
        ))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        let path = Self::manifest_path(root);
        let mut doc = Self::read_doc(root)?;

        let wrote_workspace = {
            if let Some(ws) = doc
                .get_mut("workspace")
                .and_then(Item::as_table_mut)
                .and_then(|t| t.get_mut("package"))
                .and_then(Item::as_table_mut)
            {
                if ws.contains_key("version") {
                    ws["version"] = toml_edit::value(new);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if !wrote_workspace {
            let pkg = doc
                .get_mut("package")
                .and_then(Item::as_table_mut)
                .ok_or_else(|| anyhow!("[package] missing in Cargo.toml"))?;
            pkg["version"] = toml_edit::value(new);
        }

        fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "cargo", &["generate-lockfile"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("cargo generate-lockfile".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("Cargo.toml")];
        if root.join("Cargo.lock").is_file() {
            v.push(PathBuf::from("Cargo.lock"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        let doc = Self::read_doc(root)?;
        if Self::is_workspace(&doc) {
            let Some(name) = Self::workspace_package_name(&doc) else {
                return Err(anyhow!(
                    "cannot determine [package].name for workspace publish"
                ));
            };
            super::run(root, "cargo", &["publish", "-p", &name])
        } else {
            super::run(root, "cargo", &["publish"])
        }
    }

    fn publish_command_preview(&self, root: &Path) -> Result<Option<String>> {
        let doc = Self::read_doc(root)?;
        if Self::is_workspace(&doc) {
            let name = Self::workspace_package_name(&doc)
                .ok_or_else(|| anyhow!("cannot determine [package].name for workspace publish"))?;
            Ok(Some(format!("cargo publish -p {name}")))
        } else {
            Ok(Some("cargo publish".into()))
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
}
