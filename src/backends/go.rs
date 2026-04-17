use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::backend::Backend;
use crate::git;

/// Go modules do not store a version in `go.mod` — the canonical version is
/// the git tag, so this backend only participates in the tag-and-push step.
/// The "current" version is read from the latest `v*` git tag, falling back
/// to `0.0.0` when no such tag exists.
pub struct Go;

impl Backend for Go {
    fn name(&self) -> &'static str {
        "go"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        Ok(git::latest_v_tag(root)?.unwrap_or_else(|| "0.0.0".to_owned()))
    }

    fn write_version(&self, _root: &Path, _new: &str) -> Result<()> {
        Ok(())
    }

    fn update_lockfile(&self, _root: &Path) -> Result<()> {
        Ok(())
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        None
    }

    fn files_to_stage(&self, _root: &Path) -> Vec<PathBuf> {
        Vec::new()
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
    use std::fs;

    use anyhow::Result;

    use super::*;

    #[test]
    fn tag_only_write_is_noop() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("go.mod"),
            "module example.com/demo\n\ngo 1.22\n",
        )?;
        let b = Go;
        b.write_version(tmp.path(), "0.0.1")?;
        assert_eq!(b.files_to_stage(tmp.path()).len(), 0);
        Ok(())
    }
}
