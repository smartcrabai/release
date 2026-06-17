use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpKind {
    Patch,
    Minor,
    Major,
}

/// A backend knows how to read and write the version in a project's manifest,
/// update its lockfile (if any) and publish the package (if applicable).
pub trait Backend {
    /// Human-readable backend name (matches the CLI `--backend` values).
    fn name(&self) -> &'static str;

    /// Read the current version from the manifest under `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or parsed.
    fn read_version(&self, root: &Path) -> Result<String>;

    /// Write `new` as the version in the manifest under `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be written.
    fn write_version(&self, root: &Path, new: &str) -> Result<()>;

    /// Update the lockfile (if any). No-op for backends without a lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error when the external command fails.
    fn update_lockfile(&self, root: &Path) -> Result<()>;

    /// Preview of the lockfile command for `--dry-run`. `None` for no-ops.
    fn lockfile_command_preview(&self) -> Option<String>;

    /// Files to `git add` after bumping (relative to `root`).
    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf>;

    /// Run publish. No-op for backends without a publish step.
    ///
    /// # Errors
    ///
    /// Returns an error when the external command fails.
    fn publish(&self, root: &Path) -> Result<()>;

    /// Preview of the publish command for `--dry-run`. `None` for no-ops.
    ///
    /// # Errors
    ///
    /// Returns an error when reading the manifest (needed for e.g. workspace
    /// package selection) fails.
    fn publish_command_preview(&self, root: &Path) -> Result<Option<String>>;

    /// Additional manifest files (beyond the root) that `write_version` would
    /// write for `--dry-run` logging. Relative to `root`. Default: empty.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read.
    fn additional_write_previews(&self, _root: &Path, _new: &str) -> Result<Vec<PathBuf>> {
        Ok(vec![])
    }

    /// Whether this backend has anything to publish under `root`. Used to honor
    /// native "do not publish" fields in package manifests (cargo's
    /// `[package].publish = false`, npm's `"private": true`) so that the publish
    /// step is silently skipped instead of running and failing. Returns `true`
    /// by default; backends without a meaningful publish step (go, julia) keep
    /// the default because the publish step is already a no-op for them.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or parsed.
    fn is_publishable(&self, _root: &Path) -> Result<bool> {
        Ok(true)
    }
}
