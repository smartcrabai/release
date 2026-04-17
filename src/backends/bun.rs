use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::backend::Backend;
use crate::backends::pnpm::{read_package_json_version, write_package_json_version};

pub struct Bun;

impl Backend for Bun {
    fn name(&self) -> &'static str {
        "bun"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        read_package_json_version(&root.join("package.json"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        write_package_json_version(&root.join("package.json"), new)
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "bun", &["install"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("bun install".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("package.json")];
        if root.join("bun.lock").is_file() {
            v.push(PathBuf::from("bun.lock"));
        }
        if root.join("bun.lockb").is_file() {
            v.push(PathBuf::from("bun.lockb"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        super::run(root, "bun", &["publish"])
    }

    fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
        Ok(Some("bun publish".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

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
}
