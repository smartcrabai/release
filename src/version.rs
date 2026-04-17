use anyhow::{Context, Result, anyhow};

use crate::backend::BumpKind;

/// Parse a simple `MAJOR.MINOR.PATCH` semver string and bump it according to
/// `kind`.
///
/// Only the three numeric components are supported; pre-release / build
/// metadata suffixes are rejected.
///
/// # Errors
///
/// Returns an error when `current` is not a valid `MAJOR.MINOR.PATCH`.
pub fn bump(current: &str, kind: BumpKind) -> Result<String> {
    let (major, minor, patch) = parse(current)?;
    let (major, minor, patch) = match kind {
        BumpKind::Patch => (major, minor, patch + 1),
        BumpKind::Minor => (major, minor + 1, 0),
        BumpKind::Major => (major + 1, 0, 0),
    };
    Ok(format!("{major}.{minor}.{patch}"))
}

fn parse(s: &str) -> Result<(u64, u64, u64)> {
    let trimmed = s.trim();
    let mut parts = trimmed.split('.');
    let major = parts.next().ok_or_else(|| anyhow!("missing MAJOR"))?;
    let minor = parts.next().ok_or_else(|| anyhow!("missing MINOR"))?;
    let patch = parts.next().ok_or_else(|| anyhow!("missing PATCH"))?;
    if parts.next().is_some() {
        return Err(anyhow!(
            "version '{trimmed}' has too many components (expected MAJOR.MINOR.PATCH)"
        ));
    }
    let major: u64 = major
        .parse()
        .with_context(|| format!("MAJOR '{major}' is not a number"))?;
    let minor: u64 = minor
        .parse()
        .with_context(|| format!("MINOR '{minor}' is not a number"))?;
    let patch: u64 = patch
        .parse()
        .with_context(|| format!("PATCH '{patch}' is not a number"))?;
    Ok((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_patch_rolls_units() -> Result<()> {
        assert_eq!(bump("0.0.9", BumpKind::Patch)?, "0.0.10");
        Ok(())
    }

    #[test]
    fn bump_minor_resets_patch() -> Result<()> {
        assert_eq!(bump("1.9.9", BumpKind::Minor)?, "1.10.0");
        Ok(())
    }

    #[test]
    fn bump_major_resets_minor_and_patch() -> Result<()> {
        assert_eq!(bump("1.2.3", BumpKind::Major)?, "2.0.0");
        Ok(())
    }

    #[test]
    fn bump_patch_from_zero() -> Result<()> {
        assert_eq!(bump("0.0.0", BumpKind::Patch)?, "0.0.1");
        Ok(())
    }

    #[test]
    fn reject_non_semver() {
        assert!(bump("1.2", BumpKind::Patch).is_err());
        assert!(bump("1.2.3.4", BumpKind::Patch).is_err());
        assert!(bump("abc", BumpKind::Patch).is_err());
        assert!(bump("1.2.3-rc1", BumpKind::Patch).is_err());
    }
}
