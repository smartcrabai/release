//! Project-local tool configuration (`.release.toml`).
//!
//! Distinct from manifest-native "do not publish" markers (cargo's
//! `publish = false`, npm/pnpm/bun's `"private": true`), this file lets a
//! repository permanently opt this *tool* out of running the publish step,
//! regardless of backend -- typically because publishing is handled by CI
//! (e.g. GitHub Actions OIDC trusted publishing) instead.
//!
//! The file is looked up by walking up from a starting directory towards the
//! filesystem root, stopping the search after checking the first ancestor
//! that contains a `.git` entry -- the repository's top level. This makes a
//! `.release.toml` placed at the repository root take effect even when the
//! tool is invoked from a subdirectory (e.g. a monorepo member), while a
//! `.git` boundary keeps the search from reaching past the repository into
//! unrelated ancestor directories (e.g. the user's home directory).

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use toml_edit::DocumentMut;

/// Name of the project-local config file.
pub const FILE_NAME: &str = ".release.toml";

/// Parses `text` (the contents of the `.release.toml` at `path`) and returns
/// whether publish is enabled.
fn parse_publish_flag(text: &str, path: &Path) -> Result<bool> {
    let doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;

    for (key, _) in doc.iter() {
        if key != "publish" {
            return Err(anyhow!(
                "unknown key `{key}` in {} (supported keys: publish)",
                path.display()
            ));
        }
    }

    let Some(item) = doc.get("publish") else {
        return Ok(true);
    };

    item.as_bool().ok_or_else(|| {
        anyhow!(
            "`publish` in {} must be a boolean (true or false)",
            path.display()
        )
    })
}

/// Whether the publish step is enabled for `root` via `.release.toml`.
///
/// Walks up from `root` towards the filesystem root looking for
/// `.release.toml`, checking the first ancestor that contains a `.git` entry
/// (the repository's top level) and then stopping. The first `.release.toml`
/// found is used; directories above the repository root are never consulted.
///
/// Returns `Ok(true)` (publish enabled) when no `.release.toml` is found
/// along the way, or when one is found but has no top-level `publish` key.
/// Returns the value of `publish` when the key is present.
///
/// # Errors
///
/// Returns an error when a `.release.toml` is found but:
/// - it fails to parse as TOML,
/// - it contains a top-level key other than `publish`,
/// - its `publish` key is present but is not a boolean, or
/// - it cannot be read for a reason other than not existing (e.g. it is a
///   directory rather than a file), or
/// - `root` cannot be made absolute (the current directory is gone).
pub fn is_publish_enabled(root: &Path) -> Result<bool> {
    // `Path::ancestors` is purely lexical: for the relative `.` that `run`
    // passes in it would yield only `.` and ``, never the real parent
    // directories. Make the path absolute so the walk climbs the filesystem.
    let root =
        std::path::absolute(root).with_context(|| format!("make {} absolute", root.display()))?;
    for dir in root.ancestors() {
        let path = dir.join(FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(text) => return parse_publish_flag(&text, &path),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        }

        if dir.join(".git").exists() {
            break;
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    #[test]
    fn publish_enabled_when_no_config_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        assert!(is_publish_enabled(tmp.path())?);
        Ok(())
    }

    #[test]
    fn publish_enabled_when_config_has_no_publish_key() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "")?;
        assert!(is_publish_enabled(tmp.path())?);
        Ok(())
    }

    #[test]
    fn publish_false_disables() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "publish = false\n")?;
        assert!(!is_publish_enabled(tmp.path())?);
        Ok(())
    }

    #[test]
    fn publish_true_stays_enabled() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "publish = true\n")?;
        assert!(is_publish_enabled(tmp.path())?);
        Ok(())
    }

    #[test]
    fn unknown_key_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "Publish = false\n")?;
        match is_publish_enabled(tmp.path()) {
            Err(e) => {
                assert!(format!("{e}").contains("unknown key"), "{e}");
                Ok(())
            }
            Ok(_) => Err(anyhow::anyhow!("expected an error for an unknown key")),
        }
    }

    #[test]
    fn ancestor_search_finds_parent_release_toml() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "publish = false\n")?;
        let nested = tmp.path().join("sub").join("nested");
        fs::create_dir_all(&nested)?;
        assert!(!is_publish_enabled(&nested)?);
        Ok(())
    }

    #[test]
    fn git_boundary_stops_search_at_repository_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let outer = tmp.path();
        fs::write(outer.join(FILE_NAME), "publish = false\n")?;
        let repo = outer.join("repo");
        fs::create_dir_all(repo.join(".git"))?;
        let sub = repo.join("sub");
        fs::create_dir_all(&sub)?;
        // The outer `.release.toml` must not be picked up: the search stops
        // at `repo` (which contains `.git`) before ever reaching `outer`.
        assert!(is_publish_enabled(&sub)?);
        Ok(())
    }

    #[test]
    fn release_toml_as_directory_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir(tmp.path().join(FILE_NAME))?;
        assert!(is_publish_enabled(tmp.path()).is_err());
        Ok(())
    }

    #[test]
    fn invalid_toml_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "publish = [\n")?;
        match is_publish_enabled(tmp.path()) {
            Err(e) => {
                assert!(format!("{e}").contains("parse"), "{e}");
                Ok(())
            }
            Ok(_) => Err(anyhow::anyhow!("expected an error for invalid TOML")),
        }
    }

    #[test]
    fn non_bool_publish_value_errors() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(FILE_NAME), "publish = \"no\"\n")?;
        match is_publish_enabled(tmp.path()) {
            Err(e) => {
                assert!(format!("{e}").contains("boolean"), "{e}");
                Ok(())
            }
            Ok(_) => Err(anyhow::anyhow!(
                "expected an error for non-boolean `publish`"
            )),
        }
    }
}
