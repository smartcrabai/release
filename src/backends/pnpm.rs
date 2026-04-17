use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::backend::Backend;

pub struct Pnpm;

/// Read a version from a `package.json` file.
///
/// # Errors
///
/// Returns an error when the file cannot be read or does not have a string
/// `"version"` field.
pub fn read_package_json_version(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let json: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    json.get("version")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no string \"version\" in {}", path.display()))
}

/// Write a new `version` into a `package.json` file while preserving the
/// original formatting as much as is reasonable.
///
/// # Errors
///
/// Returns an error when the file cannot be read/written.
pub fn write_package_json_version(path: &Path, new: &str) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let json: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let old = json
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("no string \"version\" in {}", path.display()))?
        .to_owned();

    // Replace the first `"version": "<old>"` occurrence (as an object member).
    // This preserves formatting of the rest of the file.
    let needle_single = format!("\"version\": \"{old}\"");
    let needle_no_space = format!("\"version\":\"{old}\"");
    let replaced = if text.contains(&needle_single) {
        text.replacen(&needle_single, &format!("\"version\": \"{new}\""), 1)
    } else if text.contains(&needle_no_space) {
        text.replacen(&needle_no_space, &format!("\"version\":\"{new}\""), 1)
    } else {
        // Fall back to re-serialising via serde_json (loses formatting).
        let mut json = json;
        if let Some(obj) = json.as_object_mut() {
            obj.insert("version".into(), Value::String(new.to_owned()));
        }
        serde_json::to_string_pretty(&json).context("serialize package.json")?
    };

    fs::write(path, replaced).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

impl Backend for Pnpm {
    fn name(&self) -> &'static str {
        "pnpm"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        read_package_json_version(&root.join("package.json"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        write_package_json_version(&root.join("package.json"), new)
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "pnpm", &["install", "--lockfile-only"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("pnpm install --lockfile-only".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("package.json")];
        if root.join("pnpm-lock.yaml").is_file() {
            v.push(PathBuf::from("pnpm-lock.yaml"));
        }
        v
    }

    fn publish(&self, root: &Path) -> Result<()> {
        super::run(root, "pnpm", &["publish", "--no-git-checks"])
    }

    fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
        Ok(Some("pnpm publish --no-git-checks".into()))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn roundtrip_formatted() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("package.json");
        fs::write(
            &path,
            "{\n  \"name\": \"demo\",\n  \"version\": \"2.3.4\"\n}\n",
        )?;
        assert_eq!(read_package_json_version(&path)?, "2.3.4");
        write_package_json_version(&path, "3.0.0")?;
        let after = fs::read_to_string(&path)?;
        assert!(after.contains("\"version\": \"3.0.0\""));
        assert!(after.contains("\"name\": \"demo\""));
        Ok(())
    }

    #[test]
    fn roundtrip_compact() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("package.json");
        fs::write(&path, "{\"name\":\"demo\",\"version\":\"1.0.0\"}")?;
        write_package_json_version(&path, "1.0.1")?;
        let after = fs::read_to_string(&path)?;
        assert!(after.contains("\"version\":\"1.0.1\""));
        Ok(())
    }
}
