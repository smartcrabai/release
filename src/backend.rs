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
}
