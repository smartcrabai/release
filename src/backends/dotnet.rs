use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::backend::Backend;

pub struct Dotnet;

/// Return the file name (relative to `root`) of the first dotnet project
/// manifest found.
fn find_project_file(root: &Path) -> Result<PathBuf> {
    if root.join("Directory.Build.props").is_file() {
        return Ok(PathBuf::from("Directory.Build.props"));
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if (ext.eq_ignore_ascii_case("csproj") || ext.eq_ignore_ascii_case("fsproj"))
            && let Some(name) = path.file_name()
        {
            return Ok(PathBuf::from(name));
        }
    }
    Err(anyhow!(
        "no .csproj / .fsproj / Directory.Build.props in {}",
        root.display()
    ))
}

fn extract_version(text: &str) -> Option<String> {
    let open = text.find("<Version>")?;
    let after = open + "<Version>".len();
    let close_rel = text[after..].find("</Version>")?;
    Some(text[after..after + close_rel].trim().to_owned())
}

fn replace_version(text: &str, old: &str, new: &str) -> Option<String> {
    let needle = format!("<Version>{old}</Version>");
    if !text.contains(&needle) {
        return None;
    }
    Some(text.replacen(&needle, &format!("<Version>{new}</Version>"), 1))
}

impl Backend for Dotnet {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        let path = root.join(find_project_file(root)?);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        extract_version(&text)
            .ok_or_else(|| anyhow!("no <Version>...</Version> in {}", path.display()))
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        let path = root.join(find_project_file(root)?);
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let old = extract_version(&text)
            .ok_or_else(|| anyhow!("no <Version>...</Version> in {}", path.display()))?;
        let replaced = replace_version(&text, &old, new)
            .ok_or_else(|| anyhow!("failed to rewrite <Version> element in {}", path.display()))?;
        fs::write(&path, replaced).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "dotnet", &["restore"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("dotnet restore".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        find_project_file(root).map(|p| vec![p]).unwrap_or_default()
    }

    fn publish(&self, root: &Path) -> Result<()> {
        super::run(root, "dotnet", &["pack", "-c", "Release"])
    }

    fn publish_command_preview(&self, _root: &Path) -> Result<Option<String>> {
        Ok(Some("dotnet pack -c Release".into()))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn roundtrip_csproj() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let proj = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>1.2.3</Version>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";
        fs::write(tmp.path().join("demo.csproj"), proj)?;
        let b = Dotnet;
        assert_eq!(b.read_version(tmp.path())?, "1.2.3");
        b.write_version(tmp.path(), "1.2.4")?;
        let after = fs::read_to_string(tmp.path().join("demo.csproj"))?;
        assert!(after.contains("<Version>1.2.4</Version>"));
        assert!(after.contains("<TargetFramework>net8.0</TargetFramework>"));
        Ok(())
    }

    #[test]
    fn roundtrip_directory_build_props() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let props = "<Project>\n  <PropertyGroup>\n    <Version>0.1.0</Version>\n  </PropertyGroup>\n</Project>\n";
        fs::write(tmp.path().join("Directory.Build.props"), props)?;
        let b = Dotnet;
        assert_eq!(b.read_version(tmp.path())?, "0.1.0");
        b.write_version(tmp.path(), "0.2.0")?;
        let after = fs::read_to_string(tmp.path().join("Directory.Build.props"))?;
        assert!(after.contains("<Version>0.2.0</Version>"));
        Ok(())
    }
}
