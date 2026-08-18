use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

/// Foreman's evidence contract: bind lane outcomes to the tree, not to the
/// model's narration. HEAD must never move; the status digest shows whether
/// real edits happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeState {
    pub head: String,
    pub status_sha256: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

pub fn capture(dir: &Path) -> Result<TreeState> {
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let status = git(dir, &["status", "--porcelain"])?;
    Ok(TreeState { head: head.trim().to_string(), status_sha256: sha256_hex(status.as_bytes()) })
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
    Ok(before.status_sha256 != after.status_sha256)
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
    fn verdict_flags_head_move_and_detects_noop() {
        let a = TreeState { head: "a".into(), status_sha256: "s1".into() };
        let same = TreeState { head: "a".into(), status_sha256: "s1".into() };
        let edited = TreeState { head: "a".into(), status_sha256: "s2".into() };
        let moved = TreeState { head: "b".into(), status_sha256: "s1".into() };
        assert!(!verdict(&a, &same).unwrap()); // silent no-op -> false
        assert!(verdict(&a, &edited).unwrap()); // real edits -> true
        assert!(verdict(&a, &moved).is_err()); // unauthorized commit
    }
}
