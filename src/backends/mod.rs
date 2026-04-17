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
use std::process::Command;

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
