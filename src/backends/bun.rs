use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::backend::Backend;
use crate::backends::pnpm::{read_package_json_version, write_package_json_version};
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
    let text =
        fs::read_to_string(&pkg_path).with_context(|| format!("read {}", pkg_path.display()))?;
    let json: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", pkg_path.display()))?;
    let patterns = extract_workspace_patterns(&json);
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    child_package_jsons(root, &patterns)
}

impl Backend for Bun {
    fn name(&self) -> &'static str {
        "bun"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        read_package_json_version(&root.join("package.json"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        write_package_json_version(&root.join("package.json"), new)?;
        for rel in bun_child_package_jsons(root)? {
            write_package_json_version(&root.join(&rel), new)
                .with_context(|| format!("update child manifest {}", rel.display()))?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "bun", &["install"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("bun install".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("package.json")];
        match bun_child_package_jsons(root) {
            Ok(children) => v.extend(children),
            Err(e) => eprintln!(
                "warning: failed to expand bun workspace children at {}: {e}",
                root.display()
            ),
        }
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
