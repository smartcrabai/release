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

    /// Skip publishing (the default; retained for cargo-release compatibility).
    #[arg(short = 'P', long, conflicts_with = "only_publish")]
    pub no_publish: bool,

    /// Only run the publish step; skip version bump, commit, tag and push.
    #[arg(short = 'p', long, conflicts_with = "no_publish")]
    pub only_publish: bool,

    /// Print the actions that would be performed without making any changes.
    #[arg(long)]
    pub dry_run: bool,

    /// Override automatic backend detection.
    #[arg(long, value_enum)]
    pub backend: Option<BackendName>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_release_never_requests_publish() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from(["release"])?;
        assert!(!cli.no_publish);
        assert!(!cli.only_publish);
        Ok(())
    }

    #[test]
    fn no_publish_compatibility_flags_are_accepted() -> Result<(), clap::Error> {
        for flag in ["-P", "--no-publish"] {
            let cli = Cli::try_parse_from(["release", flag])?;
            assert!(cli.no_publish);
            assert!(!cli.only_publish);
        }
        Ok(())
    }

    #[test]
    fn no_publish_conflicts_with_only_publish() {
        assert!(Cli::try_parse_from(["release", "-P", "--only-publish"]).is_err());
    }
}
