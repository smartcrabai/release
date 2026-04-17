use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum BumpArg {
    Patch,
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum BackendName {
    Cargo,
    Pnpm,
    Bun,
    Go,
    Dotnet,
    Julia,
    Uv,
}

/// Bump the version in a project's manifest, commit, tag, push and optionally
/// publish. Supports cargo, pnpm, bun, go, dotnet, julia and uv.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// The kind of semver bump to apply (defaults to patch).
    #[arg(value_enum, default_value_t = BumpArg::Patch)]
    pub bump: BumpArg,

    /// Skip the publish step.
    #[arg(long)]
    pub no_publish: bool,

    /// Print the actions that would be performed without making any changes.
    #[arg(long)]
    pub dry_run: bool,

    /// Override automatic backend detection.
    #[arg(long, value_enum)]
    pub backend: Option<BackendName>,
}
