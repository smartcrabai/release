//! Multi-package-manager release CLI library.
//!
//! Bumps the version in a project's manifest, commits, tags, pushes and
//! optionally publishes. Currently supports cargo, pnpm, bun, go, dotnet,
//! julia and uv.

pub mod backend;
pub mod backends;
pub mod cli;
pub mod config;
pub mod detect;
pub mod git;
pub mod version;

use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;

use crate::backend::{Backend, BumpKind};
use crate::cli::{BackendName, Cli};

/// Entry point used from `main`. Parses CLI args and runs the release flow.
///
/// # Errors
///
/// Returns an error when the release flow fails (parse, IO, git, subprocess).
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    run_with(&cli, Path::new("."))
}

/// Run the release flow against `root`. Separated from [`run`] to make testing
/// easier in the future.
///
/// # Errors
///
/// Returns an error when the release flow fails (parse, IO, git, subprocess).
pub fn run_with(cli: &Cli, root: &Path) -> Result<()> {
    let backend = select_backend(cli.backend, root)?;
    let publish_enabled = config::is_publish_enabled(root)?;

    if cli.only_publish {
        return run_publish(backend.as_ref(), root, cli.dry_run, publish_enabled);
    }

    let bump = cli.bump.into();

    // Pre-flight git validations. In dry-run we warn instead of failing.
    validate_git_state(root, cli.dry_run)?;

    if cli.dry_run {
        println!("would run: git pull --ff-only origin main");
    } else {
        git::pull_ff_only(root, "origin", "main").context("git pull --ff-only origin main")?;
    }

    let current = backend
        .read_version(root)
        .with_context(|| format!("read current version with backend '{}'", backend.name()))?;
    let new = version::bump(&current, bump)?;

    println!(
        "Bumping version: {current} -> {new} (backend: {})",
        backend.name()
    );

    if cli.dry_run {
        println!("would write: manifest version -> {new}");
        for path in backend
            .additional_write_previews(root, &new)
            .with_context(|| {
                format!(
                    "preview additional writes with backend '{}'",
                    backend.name()
                )
            })?
        {
            println!("would write: {}", path.display());
        }
    } else {
        backend
            .write_version(root, &new)
            .with_context(|| format!("write new version with backend '{}'", backend.name()))?;
    }

    if cli.dry_run {
        if let Some(cmd) = backend.lockfile_command_preview() {
            println!("would run: {cmd}");
        }
    } else {
        backend
            .update_lockfile(root)
            .with_context(|| format!("update lockfile with backend '{}'", backend.name()))?;
    }

    let commit_msg = format!("chore: bump version to {new}");
    let tag = format!("v{new}");
    let files = backend.files_to_stage(root);

    if cli.dry_run {
        if files.is_empty() {
            println!("would run: git add (nothing to stage)");
        } else {
            let joined = files
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" ");
            println!("would run: git add {joined}");
        }
        println!("would run: git commit -m \"{commit_msg}\"");
        println!("would run: git tag {tag}");
        println!("would run: git push origin main");
        println!("would run: git push origin {tag}");
    } else {
        git::add(root, &files).context("git add")?;
        // If there's nothing to stage (e.g. go backend), create an empty
        // commit so that the tag has a landing commit. Only the go backend
        // reaches this path today.
        if files.is_empty() {
            git::commit_allow_empty(root, &commit_msg).context("git commit --allow-empty")?;
        } else {
            git::commit(root, &commit_msg).context("git commit")?;
        }
        git::tag(root, &tag).context("git tag")?;
        git::push(root, "origin", "main").context("git push origin main")?;
        git::push(root, "origin", &tag).with_context(|| format!("git push origin {tag}"))?;
    }

    if cli.no_publish {
        println!("Skipping publish (--no-publish specified).");
    } else {
        run_publish(backend.as_ref(), root, cli.dry_run, publish_enabled)?;
    }

    Ok(())
}

fn run_publish(
    backend: &dyn Backend,
    root: &Path,
    dry_run: bool,
    publish_enabled: bool,
) -> Result<()> {
    if !publish_enabled {
        println!("Skipping publish: disabled by .release.toml.");
        return Ok(());
    }

    if !backend
        .is_publishable(root)
        .with_context(|| format!("check publishability with backend '{}'", backend.name()))?
    {
        println!(
            "Skipping publish: no publishable packages found (backend: {}).",
            backend.name()
        );
        return Ok(());
    }
    if dry_run {
        if let Some(cmd) = backend.publish_command_preview(root)? {
            println!("would run: {cmd}");
        } else {
            println!("No publish step for backend '{}'.", backend.name());
        }
    } else {
        backend
            .publish(root)
            .with_context(|| format!("publish with backend '{}'", backend.name()))?;
    }
    Ok(())
}

fn select_backend(requested: Option<BackendName>, root: &Path) -> Result<Box<dyn Backend>> {
    let chosen = match requested {
        Some(name) => name,
        None => detect::detect(root)?,
    };
    Ok(backends::make(chosen))
}

fn validate_git_state(root: &Path, dry_run: bool) -> Result<()> {
    if !git::is_inside_repo(root)? {
        anyhow::bail!("not inside a git repository");
    }

    let clean = git::is_clean(root)?;
    let branch = git::current_branch(root)?;
    let on_main = branch.as_deref() == Some("main");

    if dry_run {
        if !clean {
            eprintln!("warning: uncommitted changes exist (ignored in --dry-run)");
        }
        if !on_main {
            eprintln!(
                "warning: not on main branch (current: {}) (ignored in --dry-run)",
                branch.as_deref().unwrap_or("<detached>")
            );
        }
    } else {
        if !clean {
            anyhow::bail!(
                "uncommitted changes exist. Please commit or stash them before releasing."
            );
        }
        if !on_main {
            anyhow::bail!(
                "not on main branch (current: {})",
                branch.as_deref().unwrap_or("<detached>")
            );
        }
    }

    Ok(())
}

impl From<cli::BumpArg> for BumpKind {
    fn from(arg: cli::BumpArg) -> Self {
        match arg {
            cli::BumpArg::Patch => Self::Patch,
            cli::BumpArg::Minor => Self::Minor,
            cli::BumpArg::Major => Self::Major,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::{Result, anyhow};

    use super::*;

    /// A backend that is always publishable and whose `publish` /
    /// `publish_command_preview` fail loudly if invoked, so tests can prove
    /// `run_publish` skipped them (rather than them happening to succeed).
    struct AlwaysPublishableBackend;

    impl Backend for AlwaysPublishableBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn read_version(&self, _root: &Path) -> Result<String> {
            Ok("0.0.0".into())
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
            Err(anyhow!(
                "publish should not run when disabled by .release.toml"
            ))
        }

        fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
            Err(anyhow!(
                "publish preview should not run when disabled by .release.toml"
            ))
        }
    }

    #[test]
    fn run_publish_skips_when_publish_disabled() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        // Neither a real run nor a dry-run should touch publish/preview.
        run_publish(&AlwaysPublishableBackend, tmp.path(), false, false)?;
        run_publish(&AlwaysPublishableBackend, tmp.path(), true, false)?;
        Ok(())
    }

    #[test]
    fn run_publish_runs_when_publish_enabled() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let Err(err) = run_publish(&AlwaysPublishableBackend, tmp.path(), false, true) else {
            return Err(anyhow!("expected publish to run (and fail) but it did not"));
        };
        assert!(format!("{err:?}").contains("should not run"), "{err:?}");
        Ok(())
    }

    #[test]
    fn run_with_only_publish_skips_when_release_toml_disables() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )?;
        fs::write(tmp.path().join("bun.lock"), "")?;
        fs::write(tmp.path().join(config::FILE_NAME), "publish = false\n")?;

        let cli = Cli {
            bump: cli::BumpArg::Patch,
            no_publish: false,
            only_publish: true,
            dry_run: true,
            backend: Some(BackendName::Bun),
        };

        // `tmp` is not a git repository. `--only-publish` combined with
        // `--dry-run` must reach the disabled-by-`.release.toml` skip path
        // without ever needing git or running an external command.
        run_with(&cli, tmp.path())?;
        Ok(())
    }

    #[test]
    fn run_with_validates_release_toml_before_git_state() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join(config::FILE_NAME), "publish = [\n")?;

        let cli = Cli {
            bump: cli::BumpArg::Patch,
            no_publish: true,
            only_publish: false,
            dry_run: false,
            backend: Some(BackendName::Bun),
        };

        // `tmp` is not a git repository and `--no-publish` means the publish
        // step would never run anyway. If `.release.toml` were validated
        // lazily (inside the publish step, or after git validation), this
        // would either not fail at all or fail with a git error instead.
        let Err(err) = run_with(&cli, tmp.path()) else {
            return Err(anyhow!("expected an error from the invalid .release.toml"));
        };
        let message = format!("{err:?}");
        assert!(message.contains("release.toml"), "{message}");
        assert!(!message.contains("git repository"), "{message}");
        Ok(())
    }
}
