use crate::git::git;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The evidence contract: bind lane outcomes to the tree, not to the
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
/// Hashes patch content, not `git status` names: a names-only digest is
/// byte-identical when an already-dirty file is edited again, so real work
/// would read as a no-op.
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
