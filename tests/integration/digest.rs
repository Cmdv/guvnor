use guvnor::digest::{capture, sha256_hex, verdict, TreeState};
use guvnor::git;

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
    git::init_test_repo(&dir);
    git::ensure_baseline_commit(&dir).unwrap();
    // stand in for an applied impl.patch: a new, untracked file
    std::fs::write(dir.join("impl.js"), "first\n").unwrap();
    let before = capture(&dir).unwrap();
    assert_eq!(git::git(&dir, &["status", "--porcelain"]).unwrap().trim(), "A  impl.js");
    // the lane edits it — status output is identical, the content is not
    std::fs::write(dir.join("impl.js"), "second\n").unwrap();
    assert_eq!(git::git(&dir, &["status", "--porcelain"]).unwrap().trim(), "AM impl.js");
    assert!(verdict(&before, &capture(&dir).unwrap()).unwrap(), "edit read as a no-op");
    // and a lane that really did nothing still reads as nothing
    assert!(!verdict(&capture(&dir).unwrap(), &capture(&dir).unwrap()).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}
