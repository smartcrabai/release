//! Multi-package-manager release CLI library.
//!
//! Bumps the version in a project's manifest, commits, tags, pushes and
//! optionally publishes. Currently supports cargo, pnpm, bun, go, dotnet,
//! julia and uv.

pub mod backend;
pub mod backends;
pub mod cli;
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
    let bump = cli.bump.into();
    let backend = select_backend(cli.backend, root)?;

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
    } else if cli.dry_run {
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
