use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn current_branch() -> Result<String> {
    let b = git(&["branch", "--show-current"])?;
    if b.is_empty() {
        bail!("not on a branch (detached HEAD), pass the branch name explicitly");
    }
    Ok(b)
}

pub fn local_branches() -> Result<Vec<String>> {
    Ok(git(&["branch", "--format=%(refname:short)"])?
        .lines()
        .map(String::from)
        .collect())
}

/// Same for every worktree of a repo; stored in the database comment so repos sharing a server stay apart.
pub fn repo_id() -> Result<String> {
    git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
}

/// Respects worktrees and `core.hooksPath`.
pub fn hooks_dir() -> Result<PathBuf> {
    Ok(git(&["rev-parse", "--git-path", "hooks"])?.into())
}
