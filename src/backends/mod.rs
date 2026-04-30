//! Concrete backend implementations.

pub mod bun;
pub mod cargo;
pub mod dotnet;
pub mod go;
pub mod julia;
pub mod pnpm;
pub mod uv;
pub mod workspace;

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

use crate::backend::Backend;
use crate::cli::BackendName;

/// Build a concrete backend for `name`.
#[must_use]
pub fn make(name: BackendName) -> Box<dyn Backend> {
    match name {
        BackendName::Cargo => Box::new(cargo::Cargo),
        BackendName::Pnpm => Box::new(pnpm::Pnpm),
        BackendName::Bun => Box::new(bun::Bun),
        BackendName::Go => Box::new(go::Go),
        BackendName::Dotnet => Box::new(dotnet::Dotnet),
        BackendName::Julia => Box::new(julia::Julia),
        BackendName::Uv => Box::new(uv::Uv),
    }
}

/// Run a command and error out on non-zero exit.
pub(crate) fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .current_dir(root)
        .args(args)
        .status()
        .with_context(|| format!("run {program} {}", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!("{program} {} failed", args.join(" ")));
    }
    Ok(())
}

/// Ensure the user is logged in to the npm registry before publishing.
///
/// Authentication is registry-global, so callers should invoke this once per
/// publish operation rather than per-package. The login fallback is hardcoded
/// to `npm login` because bun and pnpm both read credentials from `.npmrc`.
pub(crate) fn ensure_npm_login(
    root: &Path,
    whoami_program: &str,
    whoami_args: &[&str],
) -> Result<()> {
    let status = Command::new(whoami_program)
        .current_dir(root)
        .args(whoami_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("run {whoami_program} {}", whoami_args.join(" ")))?;
    if status.success() {
        return Ok(());
    }
    eprintln!("not logged in to npm registry; running `npm login`");
    run(root, "npm", &["login"])
}
