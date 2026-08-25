use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

/// Foreman's evidence contract: bind lane outcomes to the tree, not to the
/// model's narration. HEAD must never move; the content digest shows whether
/// real edits happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeState {
    pub head: String,
    pub content_sha256: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Hash the tree's *content*, via the same patch the lane's work is captured
/// from — the two can then never disagree about whether anything happened.
///
/// This used to hash `git status --porcelain`, which is names and status codes
/// only. In a fix or rework round the implementation is already applied, so
/// every file it touches is *already* dirty and that output is byte-identical
/// before and after real edits: genuine work was failing as `*_lane_noop`.
pub fn capture(dir: &Path) -> Result<TreeState> {
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let content = crate::worktree::capture_patch(dir)?;
    Ok(TreeState { head: head.trim().to_string(), content_sha256: sha256_hex(content.as_bytes()) })
}

/// Compare before/after a lane run. Errors on unauthorized git activity;
/// returns whether the tree changed at all (silent-no-op detector).
pub fn verdict(before: &TreeState, after: &TreeState) -> Result<bool> {
    if before.head != after.head {
        bail!(
            "unauthorized git activity: HEAD moved {} -> {}",
            before.head, after.head
        );
    }
    Ok(before.content_sha256 != after.content_sha256)
}

/// True if the repo has at least one commit (HEAD resolves). A fresh
/// `git init` has none, so `git worktree add`/`rev-parse HEAD` both fail.
pub fn head_exists(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure the repo has a baseline commit. The whole loop (worktrees, evidence
/// digests, merge clean-tree check) needs a base tree; a fresh `git init` has
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_is_stable() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

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

    #[test]
    fn verdict_flags_head_move_and_detects_noop() {
        let a = TreeState { head: "a".into(), content_sha256: "s1".into() };
        let same = TreeState { head: "a".into(), content_sha256: "s1".into() };
        let edited = TreeState { head: "a".into(), content_sha256: "s2".into() };
        let moved = TreeState { head: "b".into(), content_sha256: "s1".into() };
        assert!(!verdict(&a, &same).unwrap()); // silent no-op -> false
        assert!(verdict(&a, &edited).unwrap()); // real edits -> true
        assert!(verdict(&a, &moved).is_err()); // unauthorized commit
    }

    /// The bug this digest exists to catch: editing a file that is ALREADY dirty
    /// leaves `git status --porcelain` untouched, so a content-blind digest calls
    /// real work a silent no-op and fails the run.
    #[test]
    fn capture_sees_an_edit_to_an_already_dirty_file() {
        let dir = std::env::temp_dir().join(format!("guvnor-dirty-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]).unwrap();
        ensure_baseline_commit(&dir).unwrap();
        // stand in for an applied impl.patch: a new, untracked file
        std::fs::write(dir.join("impl.js"), "first\n").unwrap();
        let before = capture(&dir).unwrap();
        assert_eq!(git(&dir, &["status", "--porcelain"]).unwrap().trim(), "A  impl.js");
        // the lane edits it — status output is identical, the content is not
        std::fs::write(dir.join("impl.js"), "second\n").unwrap();
        assert_eq!(git(&dir, &["status", "--porcelain"]).unwrap().trim(), "AM impl.js");
        assert!(verdict(&before, &capture(&dir).unwrap()).unwrap(), "edit read as a no-op");
        // and a lane that really did nothing still reads as nothing
        assert!(!verdict(&capture(&dir).unwrap(), &capture(&dir).unwrap()).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
