//! Shared helpers for monorepo-style workspaces.
//!
//! A workspace is described by a list of glob patterns pointing at package
//! directories. Each directory that matches a pattern is expected to contain
//! a well-known manifest file (e.g. `package.json`, `Cargo.toml`,
//! `pyproject.toml`), which is updated in lockstep with the root manifest.
//!
//! The glob support here is deliberately simple: only a single-segment `*` /
//! `?` / `[...]` is handled (as implemented by the `glob` crate), which is
//! enough for common patterns like `packages/*`, `apps/*` or `crates/*`.
//! Patterns using features we do not fully support (leading `!` exclusions or
//! `**` recursive wildcards) produce a warning on stderr and are skipped.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Return paths (relative to `root`) of every member manifest named
/// `manifest_name` reachable from the given workspace patterns.
///
/// Patterns are interpreted relative to `root`. Non-directory matches and
/// matches without the expected manifest are skipped silently. Duplicate
/// matches (possible when patterns overlap) are deduplicated while
/// preserving order.
///
/// # Errors
///
/// Returns an error when joining `root` with a pattern produces a non-UTF-8
/// path. Invalid patterns themselves produce a warning and are skipped.
pub fn child_manifests(
    root: &Path,
    patterns: &[String],
    manifest_name: &str,
) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for raw in patterns {
        let pat = raw.trim();
        if pat.is_empty() {
            continue;
        }
        if pat.starts_with('!') {
            eprintln!("warning: negated workspace pattern '{pat}' is not supported; skipping");
            continue;
        }
        if pat.contains("**") {
            eprintln!(
                "warning: recursive workspace pattern '{pat}' ('**') is not supported; skipping"
            );
            continue;
        }

        let joined = root.join(pat);
        let pattern_str = joined
            .to_str()
            .with_context(|| format!("non-utf8 workspace pattern: {}", joined.display()))?;

        let entries = match glob::glob(pattern_str) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("warning: invalid workspace pattern '{pat}': {err}; skipping");
                continue;
            }
        };

        for entry in entries {
            let dir = match entry {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("warning: failed to read workspace entry: {err}");
                    continue;
                }
            };
            if !dir.is_dir() {
                continue;
            }
            let manifest = dir.join(manifest_name);
            if !manifest.is_file() {
                continue;
            }
            let rel = manifest
                .strip_prefix(root)
                .unwrap_or(&manifest)
                .to_path_buf();
            if !out.iter().any(|p| p == &rel) {
                out.push(rel);
            }
        }
    }
    Ok(out)
}

/// Convenience wrapper for JS-style workspaces that use `package.json`.
///
/// # Errors
///
/// See [`child_manifests`].
pub fn child_package_jsons(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    child_manifests(root, patterns, "package.json")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    #[test]
    fn expands_single_level_star() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("packages/a"))?;
        fs::create_dir_all(tmp.path().join("packages/b"))?;
        fs::write(tmp.path().join("packages/a/package.json"), "{}")?;
        fs::write(tmp.path().join("packages/b/package.json"), "{}")?;

        let got = child_package_jsons(tmp.path(), &["packages/*".into()])?;
        let mut got_strs: Vec<String> = got.iter().map(|p| p.display().to_string()).collect();
        got_strs.sort();
        assert_eq!(
            got_strs,
            vec![
                "packages/a/package.json".to_owned(),
                "packages/b/package.json".to_owned()
            ]
        );
        Ok(())
    }

    #[test]
    fn skips_dirs_without_package_json() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("packages/a"))?;
        fs::create_dir_all(tmp.path().join("packages/empty"))?;
        fs::write(tmp.path().join("packages/a/package.json"), "{}")?;

        let got = child_package_jsons(tmp.path(), &["packages/*".into()])?;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], PathBuf::from("packages/a/package.json"));
        Ok(())
    }

    #[test]
    fn deduplicates_overlapping_patterns() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("packages/a"))?;
        fs::write(tmp.path().join("packages/a/package.json"), "{}")?;

        let got = child_package_jsons(tmp.path(), &["packages/*".into(), "packages/a".into()])?;
        assert_eq!(got.len(), 1);
        Ok(())
    }

    #[test]
    fn warns_and_skips_unsupported_patterns() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("packages/a"))?;
        fs::write(tmp.path().join("packages/a/package.json"), "{}")?;

        let got = child_package_jsons(
            tmp.path(),
            &["!**/test/**".into(), "**/foo".into(), "packages/*".into()],
        )?;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], PathBuf::from("packages/a/package.json"));
        Ok(())
    }

    #[test]
    fn child_manifests_works_with_cargo_toml() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("crates/a"))?;
        fs::create_dir_all(tmp.path().join("crates/b"))?;
        fs::write(tmp.path().join("crates/a/Cargo.toml"), "[package]\n")?;
        fs::write(tmp.path().join("crates/b/Cargo.toml"), "[package]\n")?;

        let got = child_manifests(tmp.path(), &["crates/*".into()], "Cargo.toml")?;
        let mut got_strs: Vec<String> = got.iter().map(|p| p.display().to_string()).collect();
        got_strs.sort();
        assert_eq!(
            got_strs,
            vec![
                "crates/a/Cargo.toml".to_owned(),
                "crates/b/Cargo.toml".to_owned()
            ]
        );
        Ok(())
    }

    #[test]
    fn child_manifests_works_with_pyproject_toml() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("packages/a"))?;
        fs::create_dir_all(tmp.path().join("packages/b"))?;
        fs::write(tmp.path().join("packages/a/pyproject.toml"), "[project]\n")?;
        fs::write(tmp.path().join("packages/b/pyproject.toml"), "[project]\n")?;

        let got = child_manifests(tmp.path(), &["packages/*".into()], "pyproject.toml")?;
        assert_eq!(got.len(), 2);
        Ok(())
    }
}
