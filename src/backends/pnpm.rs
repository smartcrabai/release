use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::backend::Backend;
use crate::backends::workspace::child_package_jsons;

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

/// Parse the `packages:` list from a `pnpm-workspace.yaml` file.
///
/// A deliberately small hand-rolled parser: we scan for a top-level
/// `packages:` key and collect the subsequent `- "..."` / `- '...'` / `- bare`
/// sequence entries until a line starts at a non-indented position. This
/// avoids pulling in a YAML dependency just for reading workspace patterns.
fn parse_pnpm_workspace_patterns(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_packages = false;
    let mut packages_indent: usize = 0;
    for raw in text.lines() {
        // Strip `#` comments. We accept that `#` inside quoted values would
        // also be stripped — workspace files rarely use such patterns.
        let line = raw.split('#').next().unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let content = line.trim_start();

        if !in_packages {
            if content.starts_with("packages:") {
                in_packages = true;
                packages_indent = indent;
            }
            continue;
        }

        // Leaving the `packages:` block when we encounter another top-level
        // key at the same indentation.
        if indent <= packages_indent && !content.starts_with('-') {
            in_packages = false;
            continue;
        }

        if let Some(rest) = content.strip_prefix('-') {
            let value = rest.trim();
            if value.is_empty() {
                continue;
            }
            out.push(strip_yaml_quotes(value).to_owned());
        }
    }
    out
}

fn strip_yaml_quotes(s: &str) -> &str {
    if let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner;
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner;
    }
    s
}

/// Read `pnpm-workspace.yaml` if present and return the resolved child
/// `package.json` paths (relative to `root`).
fn pnpm_child_package_jsons(root: &Path) -> Result<Vec<PathBuf>> {
    let ws_path = root.join("pnpm-workspace.yaml");
    if !ws_path.is_file() {
        return Ok(Vec::new());
    }
    let text =
        fs::read_to_string(&ws_path).with_context(|| format!("read {}", ws_path.display()))?;
    let patterns = parse_pnpm_workspace_patterns(&text);
    child_package_jsons(root, &patterns)
}

impl Backend for Pnpm {
    fn name(&self) -> &'static str {
        "pnpm"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        read_package_json_version(&root.join("package.json"))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        write_package_json_version(&root.join("package.json"), new)?;
        for rel in pnpm_child_package_jsons(root)? {
            write_package_json_version(&root.join(&rel), new)
                .with_context(|| format!("update child manifest {}", rel.display()))?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "pnpm", &["install", "--lockfile-only"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("pnpm install --lockfile-only".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from("package.json")];
        if let Ok(children) = pnpm_child_package_jsons(root) {
            v.extend(children);
        }
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

    #[test]
    fn parse_packages_from_simple_yaml() {
        let yaml = "packages:\n  - \"packages/*\"\n  - 'apps/*'\n  - crates/*\n";
        let got = parse_pnpm_workspace_patterns(yaml);
        assert_eq!(
            got,
            vec![
                "packages/*".to_owned(),
                "apps/*".to_owned(),
                "crates/*".to_owned()
            ]
        );
    }

    #[test]
    fn parse_packages_stops_at_other_top_level_key() {
        let yaml = "packages:\n  - packages/*\nonlyBuiltDependencies:\n  - esbuild\n";
        let got = parse_pnpm_workspace_patterns(yaml);
        assert_eq!(got, vec!["packages/*".to_owned()]);
    }

    #[test]
    fn workspace_write_updates_root_and_children() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();

        fs::write(
            root.join("package.json"),
            "{\n  \"name\": \"root\",\n  \"version\": \"1.0.0\",\n  \"private\": true\n}\n",
        )?;
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - \"packages/*\"\n",
        )?;
        fs::create_dir_all(root.join("packages/a"))?;
        fs::create_dir_all(root.join("packages/b"))?;
        fs::write(
            root.join("packages/a/package.json"),
            "{ \"name\": \"@x/a\", \"version\": \"1.0.0\" }\n",
        )?;
        fs::write(
            root.join("packages/b/package.json"),
            "{ \"name\": \"@x/b\", \"version\": \"1.0.0\" }\n",
        )?;

        let backend = Pnpm;
        backend.write_version(root, "1.1.0")?;

        assert_eq!(backend.read_version(root)?, "1.1.0");
        assert_eq!(
            read_package_json_version(&root.join("packages/a/package.json"))?,
            "1.1.0"
        );
        assert_eq!(
            read_package_json_version(&root.join("packages/b/package.json"))?,
            "1.1.0"
        );

        let staged = backend.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("package.json")));
        assert!(staged.contains(&PathBuf::from("packages/a/package.json")));
        assert!(staged.contains(&PathBuf::from("packages/b/package.json")));
        Ok(())
    }
}
