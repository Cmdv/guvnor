use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("git {args:?} failed to spawn"))?;
    if !out.status.success() {
        bail!(
            "git {:?} in {} failed: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True if the repo has at least one commit (HEAD resolves). A fresh
/// `git init` has none, so `git worktree add`/`rev-parse HEAD` both fail.
fn head_exists(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure the repo has a baseline commit. The whole loop (worktrees, evidence
/// digests, stage clean-tree check) needs a base tree; a fresh `git init` has
/// none. Guv'nor bootstraps one from the current tree so a brand-new repo can
/// run — the human still owns every *later* commit. Returns whether it created
/// one. `.guvnor/runs/` stays out via the .gitignore init writes.
pub fn ensure_baseline_commit(repo: &Path) -> Result<bool> {
    if head_exists(repo) {
        return Ok(false);
    }
    git(repo, &["add", "-A"])?;
    git(repo, &["commit", "--allow-empty", "-m", "guvnor: baseline commit"])
        .context("could not create the baseline commit (is git user.name/user.email set?)")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_baseline_commit_bootstraps_fresh_repo() {
        let dir = std::env::temp_dir().join(format!("guvnor-baseline-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]).unwrap();
        // commit identity comes from the ambient git config, same as real use
        assert!(!head_exists(&dir)); // fresh init: no HEAD
        assert!(ensure_baseline_commit(&dir).unwrap()); // creates it
        assert!(head_exists(&dir));
        assert!(!ensure_baseline_commit(&dir).unwrap()); // idempotent
        std::fs::remove_dir_all(&dir).ok();
    }
}
