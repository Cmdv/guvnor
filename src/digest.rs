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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::ensure_baseline_commit;

    #[test]
    fn sha_is_stable() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
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

    /// Editing a file that is ALREADY dirty must still register as an edit —
    /// the scenario `capture`'s content hash exists for.
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
