use guvnor::git::{ensure_baseline_commit, head_exists, init_test_repo};

#[test]
fn ensure_baseline_commit_bootstraps_fresh_repo() {
    let dir = std::env::temp_dir().join(format!("guvnor-baseline-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    init_test_repo(&dir);
    assert!(!head_exists(&dir)); // fresh init: no HEAD
    assert!(ensure_baseline_commit(&dir).unwrap()); // creates it
    assert!(head_exists(&dir));
    assert!(!ensure_baseline_commit(&dir).unwrap()); // idempotent
    std::fs::remove_dir_all(&dir).ok();
}
