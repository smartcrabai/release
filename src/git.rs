use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};

/// Returns `true` if `root` is inside a git repository.
///
/// # Errors
///
/// Returns an error when invoking `git` fails.
pub fn is_inside_repo(root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .context("run git rev-parse")?;
    Ok(output.status.success())
}

/// Returns `true` when the working tree and index have no changes.
///
/// # Errors
///
/// Returns an error when invoking `git` fails.
pub fn is_clean(root: &Path) -> Result<bool> {
    let worktree = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--quiet"])
        .status()
        .context("run git diff --quiet")?;
    let index = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .context("run git diff --cached --quiet")?;
    Ok(worktree.success() && index.success())
}

/// Returns the name of the current branch, or `None` when detached.
///
/// # Errors
///
/// Returns an error when invoking `git` fails.
pub fn current_branch(root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .context("run git symbolic-ref")?;
    if !output.status.success() {
        return Ok(None);
    }
    let name = String::from_utf8(output.stdout)
        .context("decode branch name as utf-8")?
        .trim()
        .to_owned();
    if name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(name))
    }
}

/// Fast-forward pull from `remote`/`branch`.
///
/// # Errors
///
/// Returns an error when `git pull --ff-only` fails.
pub fn pull_ff_only(root: &Path, remote: &str, branch: &str) -> Result<()> {
    run_git(root, &["pull", "--ff-only", remote, branch])
}

/// Stage `files` relative to `root`.
///
/// # Errors
///
/// Returns an error when `git add` fails.
pub fn add(root: &Path, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    let strs: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    for s in &strs {
        args.push(s);
    }
    run_git(root, &args)
}

/// Create a commit with `message`.
///
/// # Errors
///
/// Returns an error when `git commit` fails.
pub fn commit(root: &Path, message: &str) -> Result<()> {
    run_git(root, &["commit", "-m", message])
}

/// Create a commit with `message`, allowing an empty commit. Used for
/// backends (like `go`) that only care about tagging.
///
/// # Errors
///
/// Returns an error when `git commit --allow-empty` fails.
pub fn commit_allow_empty(root: &Path, message: &str) -> Result<()> {
    run_git(root, &["commit", "--allow-empty", "-m", message])
}

/// Create a lightweight tag `tag`.
///
/// # Errors
///
/// Returns an error when `git tag` fails.
pub fn tag(root: &Path, tag: &str) -> Result<()> {
    run_git(root, &["tag", tag])
}

/// Push `refspec` to `remote`.
///
/// # Errors
///
/// Returns an error when `git push` fails.
pub fn push(root: &Path, remote: &str, refspec: &str) -> Result<()> {
    run_git(root, &["push", remote, refspec])
}

/// Return the latest `v*` tag with the leading `v` stripped (e.g. `v1.2.3`
/// becomes `"1.2.3"`). Returns `None` when no matching tag exists.
///
/// # Errors
///
/// Returns an error when invoking `git` fails (but not when there simply is
/// no matching tag).
pub fn latest_v_tag(root: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["describe", "--tags", "--abbrev=0", "--match", "v*"])
        .output()
        .context("run git describe")?;
    if !out.status.success() {
        return Ok(None);
    }
    let raw = String::from_utf8(out.stdout)
        .context("decode git describe output")?
        .trim()
        .to_owned();
    Ok(Some(raw.strip_prefix('v').unwrap_or(&raw).to_owned()))
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!("git {} failed", args.join(" ")));
    }
    Ok(())
}
