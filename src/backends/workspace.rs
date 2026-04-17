//! Shared helpers for JS monorepo-style workspaces (pnpm / bun).
//!
//! A workspace is described by a list of glob patterns pointing at package
//! directories. Each directory that matches a pattern is expected to contain a
//! `package.json`, which is updated in lockstep with the root manifest.
//!
//! The glob support here is deliberately simple: only a single-segment `*` /
//! `?` / `[...]` is handled (as implemented by the `glob` crate), which is
//! enough for common patterns like `packages/*`, `apps/*` or `crates/*`.
//! Patterns using features we do not fully support (leading `!` exclusions or
//! `**` recursive wildcards) produce a warning on stderr and are skipped.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Return paths (relative to `root`) of every `package.json` reachable from
/// the given workspace patterns, excluding the root `package.json` itself.
///
/// Patterns are interpreted relative to `root`. Non-directory matches and
/// matches without a `package.json` are skipped silently. Duplicate matches
/// (possible when patterns overlap) are deduplicated while preserving order.
///
/// # Errors
///
/// Returns an error when expanding a pattern fails with an unexpected I/O
/// error. Invalid patterns themselves produce a warning and are skipped.
pub fn child_package_jsons(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
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
            let pkg = dir.join("package.json");
            if !pkg.is_file() {
                continue;
            }
            let rel = pkg.strip_prefix(root).unwrap_or(&pkg).to_path_buf();
            if !out.iter().any(|p| p == &rel) {
                out.push(rel);
            }
        }
    }
    Ok(out)
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
}
