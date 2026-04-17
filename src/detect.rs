use std::path::Path;

use anyhow::{Result, anyhow};

use crate::cli::BackendName;

/// Detect which backend to use for the project rooted at `root`.
///
/// Priority order (first match wins):
/// 1. `Cargo.toml` -> cargo
/// 2. `pyproject.toml` + `uv.lock` -> uv
/// 3. `package.json` + `pnpm-lock.yaml` -> pnpm
/// 4. `package.json` + (`bun.lock` or `bun.lockb`) -> bun
/// 5. `package.json` alone -> pnpm (fallback)
/// 6. `Project.toml` containing `uuid` and `version` -> julia
/// 7. `*.sln` / `*.csproj` / `*.fsproj` / `Directory.Build.props` -> dotnet
/// 8. `go.mod` -> go
///
/// # Errors
///
/// Returns an error when no known manifest is found under `root` or the
/// directory cannot be read.
pub fn detect(root: &Path) -> Result<BackendName> {
    if root.join("Cargo.toml").is_file() {
        return Ok(BackendName::Cargo);
    }

    let has_pyproject = root.join("pyproject.toml").is_file();
    if has_pyproject && root.join("uv.lock").is_file() {
        return Ok(BackendName::Uv);
    }

    let has_package_json = root.join("package.json").is_file();
    if has_package_json {
        if root.join("pnpm-lock.yaml").is_file() {
            return Ok(BackendName::Pnpm);
        }
        if root.join("bun.lock").is_file() || root.join("bun.lockb").is_file() {
            return Ok(BackendName::Bun);
        }
        return Ok(BackendName::Pnpm);
    }

    if is_julia_project(root)? {
        return Ok(BackendName::Julia);
    }

    if has_dotnet_project(root)? {
        return Ok(BackendName::Dotnet);
    }

    if root.join("go.mod").is_file() {
        return Ok(BackendName::Go);
    }

    Err(anyhow!(
        "could not detect a supported package manager in {}",
        root.display()
    ))
}

fn is_julia_project(root: &Path) -> Result<bool> {
    let path = root.join("Project.toml");
    if !path.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path)?;
    let has_uuid = text.lines().any(|l| l.trim_start().starts_with("uuid"));
    let has_version = text.lines().any(|l| l.trim_start().starts_with("version"));
    Ok(has_uuid && has_version)
}

fn has_dotnet_project(root: &Path) -> Result<bool> {
    if root.join("Directory.Build.props").is_file() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext.eq_ignore_ascii_case("csproj")
            || ext.eq_ignore_ascii_case("fsproj")
            || ext.eq_ignore_ascii_case("sln")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    fn write(root: &Path, name: &str, contents: &str) -> Result<()> {
        fs::write(root.join(name), contents)?;
        Ok(())
    }

    #[test]
    fn detects_cargo() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(
            tmp.path(),
            "Cargo.toml",
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )?;
        assert_eq!(detect(tmp.path())?, BackendName::Cargo);
        Ok(())
    }

    #[test]
    fn detects_uv() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(
            tmp.path(),
            "pyproject.toml",
            "[project]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )?;
        write(tmp.path(), "uv.lock", "")?;
        assert_eq!(detect(tmp.path())?, BackendName::Uv);
        Ok(())
    }

    #[test]
    fn detects_pnpm() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "package.json", "{\"version\":\"1.0.0\"}")?;
        write(tmp.path(), "pnpm-lock.yaml", "")?;
        assert_eq!(detect(tmp.path())?, BackendName::Pnpm);
        Ok(())
    }

    #[test]
    fn detects_bun_lock() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "package.json", "{\"version\":\"1.0.0\"}")?;
        write(tmp.path(), "bun.lock", "")?;
        assert_eq!(detect(tmp.path())?, BackendName::Bun);
        Ok(())
    }

    #[test]
    fn detects_bun_lockb() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "package.json", "{\"version\":\"1.0.0\"}")?;
        write(tmp.path(), "bun.lockb", "")?;
        assert_eq!(detect(tmp.path())?, BackendName::Bun);
        Ok(())
    }

    #[test]
    fn package_json_alone_falls_back_to_pnpm() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "package.json", "{\"version\":\"1.0.0\"}")?;
        assert_eq!(detect(tmp.path())?, BackendName::Pnpm);
        Ok(())
    }

    #[test]
    fn detects_julia() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(
            tmp.path(),
            "Project.toml",
            "name = \"Demo\"\nuuid = \"12345678-1234-5678-1234-567812345678\"\nversion = \"0.1.0\"\n",
        )?;
        assert_eq!(detect(tmp.path())?, BackendName::Julia);
        Ok(())
    }

    #[test]
    fn detects_dotnet_csproj() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "demo.csproj", "<Project></Project>")?;
        assert_eq!(detect(tmp.path())?, BackendName::Dotnet);
        Ok(())
    }

    #[test]
    fn detects_dotnet_fsproj() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "demo.fsproj", "<Project></Project>")?;
        assert_eq!(detect(tmp.path())?, BackendName::Dotnet);
        Ok(())
    }

    #[test]
    fn detects_dotnet_directory_build_props() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "Directory.Build.props", "<Project></Project>")?;
        assert_eq!(detect(tmp.path())?, BackendName::Dotnet);
        Ok(())
    }

    #[test]
    fn detects_dotnet_sln() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "Solution.sln", "")?;
        assert_eq!(detect(tmp.path())?, BackendName::Dotnet);
        Ok(())
    }

    #[test]
    fn detects_go() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(tmp.path(), "go.mod", "module example.com/demo\n\ngo 1.22\n")?;
        assert_eq!(detect(tmp.path())?, BackendName::Go);
        Ok(())
    }

    #[test]
    fn cargo_wins_over_others() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write(
            tmp.path(),
            "Cargo.toml",
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )?;
        write(tmp.path(), "go.mod", "module x\n")?;
        assert_eq!(detect(tmp.path())?, BackendName::Cargo);
        Ok(())
    }

    #[test]
    fn errors_on_empty_dir() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        assert!(detect(tmp.path()).is_err());
        Ok(())
    }
}
