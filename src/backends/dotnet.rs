use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::backend::Backend;

pub struct Dotnet;

/// One of three supported layouts for a .NET project tree.
enum Layout {
    /// A single `Directory.Build.props` that holds a centrally-managed
    /// `<Version>` (even if child `.csproj` / `.fsproj` files exist, they are
    /// assumed to inherit the version and are left untouched).
    CentralizedProps,
    /// A `.sln` at the root referencing one or more project files.
    Solution { projects: Vec<PathBuf> },
    /// No solution; project files are discovered recursively.
    Projects { projects: Vec<PathBuf> },
}

fn has_version_element(text: &str) -> bool {
    extract_version(text).is_some()
}

fn extract_version(text: &str) -> Option<String> {
    let open = text.find("<Version>")?;
    let after = open + "<Version>".len();
    let close_rel = text[after..].find("</Version>")?;
    Some(text[after..after + close_rel].trim().to_owned())
}

fn replace_version(text: &str, old: &str, new: &str) -> Option<String> {
    let open_tag = "<Version>";
    let close_tag = "</Version>";
    let open = text.find(open_tag)?;
    let after = open + open_tag.len();
    let close_rel = text[after..].find(close_tag)?;
    let inner = &text[after..after + close_rel];
    if inner.trim() != old {
        return None;
    }
    let end = after + close_rel + close_tag.len();
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..open]);
    out.push_str(open_tag);
    out.push_str(new);
    out.push_str(close_tag);
    out.push_str(&text[end..]);
    Some(out)
}

/// Very small solution parser: extract the second quoted string on each
/// `Project(...) = ` line, which is the relative project path.
fn parse_sln_project_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("Project(") {
            continue;
        }
        // Collect each quoted segment.
        let mut quoted: Vec<&str> = Vec::new();
        let mut in_quote = false;
        let mut start = 0usize;
        for (idx, ch) in line.char_indices() {
            if ch == '"' {
                if in_quote {
                    quoted.push(&line[start..idx]);
                    in_quote = false;
                } else {
                    in_quote = true;
                    start = idx + 1;
                }
            }
        }
        // 0: project-type guid, 1: name, 2: relative path, 3: project guid.
        if let Some(rel) = quoted.get(2) {
            let lower = rel.to_lowercase();
            if lower.ends_with(".csproj") || lower.ends_with(".fsproj") {
                // .sln uses backslashes on Windows. Normalize for our purposes.
                let normalized = rel.replace('\\', "/");
                out.push(PathBuf::from(normalized));
            }
        }
    }
    out
}

fn find_sln(root: &Path) -> Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext.eq_ignore_ascii_case("sln")
            && let Some(name) = path.file_name()
        {
            return Ok(Some(PathBuf::from(name)));
        }
    }
    Ok(None)
}

/// Recursively collect `.csproj` / `.fsproj` files under `root`.
fn discover_projects_recursively(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    // Skip common vendor / build dirs to keep discovery fast.
    let skip = |name: &str| {
        matches!(
            name,
            "bin" | "obj" | ".git" | "node_modules" | "target" | ".vs"
        )
    };
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if skip(name_str) {
                continue;
            }
            walk(root, &path, out)?;
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext.eq_ignore_ascii_case("csproj") || ext.eq_ignore_ascii_case("fsproj") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.push(rel);
        }
    }
    Ok(())
}

fn classify(root: &Path) -> Result<Layout> {
    // 1. Centralized Directory.Build.props with <Version>.
    let props_path = root.join("Directory.Build.props");
    if props_path.is_file() {
        let text = fs::read_to_string(&props_path)
            .with_context(|| format!("read {}", props_path.display()))?;
        if has_version_element(&text) {
            return Ok(Layout::CentralizedProps);
        }
    }

    // 2. .sln at the root.
    if let Some(sln) = find_sln(root)? {
        let sln_path = root.join(&sln);
        let text = fs::read_to_string(&sln_path)
            .with_context(|| format!("read {}", sln_path.display()))?;
        let projects = parse_sln_project_paths(&text);
        if !projects.is_empty() {
            return Ok(Layout::Solution { projects });
        }
    }

    // 3. Recursive project discovery.
    let projects = discover_projects_recursively(root)?;
    if projects.is_empty() {
        return Err(anyhow!(
            "no .csproj / .fsproj / Directory.Build.props in {}",
            root.display()
        ));
    }
    Ok(Layout::Projects { projects })
}

fn read_version_from(path: &Path) -> Result<Option<String>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(extract_version(&text))
}

fn update_version_in(path: &Path, new: &str) -> Result<bool> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let Some(old) = extract_version(&text) else {
        return Ok(false);
    };
    let Some(replaced) = replace_version(&text, &old, new) else {
        return Err(anyhow!(
            "failed to rewrite <Version> element in {}",
            path.display()
        ));
    };
    fs::write(path, replaced).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

impl Backend for Dotnet {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn read_version(&self, root: &Path) -> Result<String> {
        match classify(root)? {
            Layout::CentralizedProps => {
                let path = root.join("Directory.Build.props");
                read_version_from(&path)?
                    .ok_or_else(|| anyhow!("no <Version>...</Version> in {}", path.display()))
            }
            Layout::Solution { projects } | Layout::Projects { projects } => {
                for rel in &projects {
                    let abs = root.join(rel);
                    if let Some(v) = read_version_from(&abs)? {
                        return Ok(v);
                    }
                }
                Err(anyhow!(
                    "no <Version>...</Version> found in any project file"
                ))
            }
        }
    }

    fn write_version(&self, root: &Path, new: &str) -> Result<()> {
        match classify(root)? {
            Layout::CentralizedProps => {
                let path = root.join("Directory.Build.props");
                if !update_version_in(&path, new)? {
                    return Err(anyhow!("no <Version>...</Version> in {}", path.display()));
                }
                Ok(())
            }
            Layout::Solution { projects } | Layout::Projects { projects } => {
                let mut any = false;
                for rel in &projects {
                    let abs = root.join(rel);
                    if !abs.is_file() {
                        continue;
                    }
                    match update_version_in(&abs, new) {
                        Ok(true) => any = true,
                        Ok(false) => eprintln!(
                            "warning: {} has no <Version> element; skipping",
                            rel.display()
                        ),
                        Err(err) => return Err(err),
                    }
                }
                if !any {
                    return Err(anyhow!(
                        "no <Version> element found in any discovered project"
                    ));
                }
                Ok(())
            }
        }
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        super::run(root, "dotnet", &["restore"])
    }

    fn lockfile_command_preview(&self) -> Option<String> {
        Some("dotnet restore".into())
    }

    fn files_to_stage(&self, root: &Path) -> Vec<PathBuf> {
        let Ok(layout) = classify(root) else {
            return Vec::new();
        };
        match layout {
            Layout::CentralizedProps => vec![PathBuf::from("Directory.Build.props")],
            Layout::Solution { projects } | Layout::Projects { projects } => {
                // Only stage projects that actually contain a <Version>.
                projects
                    .into_iter()
                    .filter(|rel| {
                        let abs = root.join(rel);
                        fs::read_to_string(&abs).is_ok_and(|t| has_version_element(&t))
                    })
                    .collect()
            }
        }
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

    #[test]
    fn directory_build_props_does_not_touch_children() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::write(
            root.join("Directory.Build.props"),
            "<Project><PropertyGroup><Version>2.0.0</Version></PropertyGroup></Project>",
        )?;
        fs::create_dir_all(root.join("Apps/X"))?;
        fs::write(
            root.join("Apps/X/X.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>",
        )?;

        let b = Dotnet;
        b.write_version(root, "2.1.0")?;
        let staged = b.files_to_stage(root);
        assert_eq!(staged, vec![PathBuf::from("Directory.Build.props")]);
        let child = fs::read_to_string(root.join("Apps/X/X.csproj"))?;
        // Child untouched: still no <Version> element.
        assert!(!child.contains("<Version>"));
        Ok(())
    }

    #[test]
    fn sln_updates_all_referenced_projects() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir_all(root.join("A"))?;
        fs::create_dir_all(root.join("B"))?;
        fs::write(
            root.join("A/A.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><Version>1.0.0</Version><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>",
        )?;
        fs::write(
            root.join("B/B.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><Version>1.0.0</Version><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>",
        )?;
        let sln = "Microsoft Visual Studio Solution File, Format Version 12.00\n\
Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"A\", \"A/A.csproj\", \"{11111111-1111-1111-1111-111111111111}\"\nEndProject\n\
Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"B\", \"B/B.csproj\", \"{22222222-2222-2222-2222-222222222222}\"\nEndProject\n";
        fs::write(root.join("Solution.sln"), sln)?;

        let b = Dotnet;
        assert_eq!(b.read_version(root)?, "1.0.0");
        b.write_version(root, "1.1.0")?;
        let a = fs::read_to_string(root.join("A/A.csproj"))?;
        let bs = fs::read_to_string(root.join("B/B.csproj"))?;
        assert!(a.contains("<Version>1.1.0</Version>"));
        assert!(bs.contains("<Version>1.1.0</Version>"));

        let staged = b.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("A/A.csproj")));
        assert!(staged.contains(&PathBuf::from("B/B.csproj")));
        Ok(())
    }

    #[test]
    fn recursive_discovery_without_sln() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir_all(root.join("libs/one"))?;
        fs::create_dir_all(root.join("libs/two"))?;
        fs::write(
            root.join("libs/one/One.csproj"),
            "<Project><PropertyGroup><Version>0.1.0</Version></PropertyGroup></Project>",
        )?;
        fs::write(
            root.join("libs/two/Two.fsproj"),
            "<Project><PropertyGroup><Version>0.1.0</Version></PropertyGroup></Project>",
        )?;

        let b = Dotnet;
        b.write_version(root, "0.2.0")?;
        let one = fs::read_to_string(root.join("libs/one/One.csproj"))?;
        let two = fs::read_to_string(root.join("libs/two/Two.fsproj"))?;
        assert!(one.contains("<Version>0.2.0</Version>"));
        assert!(two.contains("<Version>0.2.0</Version>"));
        Ok(())
    }

    #[test]
    fn parse_sln_paths_handles_backslashes() {
        let text = "Project(\"{GUID}\") = \"Foo\", \"sub\\Foo.csproj\", \"{G2}\"\nEndProject\n";
        let got = parse_sln_project_paths(text);
        assert_eq!(got, vec![PathBuf::from("sub/Foo.csproj")]);
    }

    #[test]
    fn sln_skips_project_without_version_element() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir_all(root.join("A"))?;
        fs::create_dir_all(root.join("B"))?;
        // A has <Version>, B does not.
        fs::write(
            root.join("A/A.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><Version>1.0.0</Version></PropertyGroup></Project>",
        )?;
        fs::write(
            root.join("B/B.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>",
        )?;
        let sln = "Project(\"{G}\") = \"A\", \"A/A.csproj\", \"{G1}\"\nEndProject\n\
Project(\"{G}\") = \"B\", \"B/B.csproj\", \"{G2}\"\nEndProject\n";
        fs::write(root.join("Solution.sln"), sln)?;

        let b = Dotnet;
        b.write_version(root, "1.1.0")?;

        let staged = b.files_to_stage(root);
        assert!(staged.contains(&PathBuf::from("A/A.csproj")));
        assert!(!staged.contains(&PathBuf::from("B/B.csproj")));
        Ok(())
    }

    #[test]
    fn replace_version_tolerates_whitespace_inside_tags() -> Result<()> {
        let text =
            "<Project><PropertyGroup><Version>  1.2.3 \n</Version></PropertyGroup></Project>";
        assert_eq!(extract_version(text).as_deref(), Some("1.2.3"));
        let out = replace_version(text, "1.2.3", "1.2.4")
            .ok_or_else(|| anyhow!("replace_version returned None"))?;
        assert!(out.contains("<Version>1.2.4</Version>"));
        assert!(!out.contains("1.2.3"));
        Ok(())
    }
}
