use guvnor::git::git;
use guvnor::worktree::{
    capture_patch, ensure_wt_ignored, is_run_wt, overlapping_paths, patch_paths, validate_patch,
    wt_container,
};

const PATCH: &str = "diff --git a/test/A.hs b/test/A.hs\nnew file mode 100644\n--- /dev/null\n+++ b/test/A.hs\n@@\n+x\ndiff --git a/src/B.hs b/src/B.hs\n--- a/src/B.hs\n+++ b/src/B.hs\n@@\n+y\n";

#[test]
fn extracts_unique_paths() {
    assert_eq!(patch_paths(PATCH), vec!["test/A.hs".to_string(), "src/B.hs".to_string()]);
}

/// The same escape driven through the real `capture_patch`, since the flag
/// that closes it lives there: rename detection is on by default, and a
/// lane can reach `git mv` (it is not in hookguard's BANNED list).
#[test]
fn capture_patch_cannot_hide_a_rename_from_the_fence() {
    let dir = std::env::temp_dir().join(format!("guvnor-rename-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join(".guvnor/runs/x")).unwrap();
    guvnor::git::init_test_repo(&dir);
    std::fs::write(dir.join("src/keep.txt"), "keep me\n").unwrap();
    git(&dir, &["add", "-A"]).unwrap();
    git(&dir, &["commit", "-qm", "base"]).unwrap();
    // post a file into guvnor's own evidence directory, as a rename
    git(&dir, &["mv", "src/keep.txt", ".guvnor/runs/x/review.json"]).unwrap();
    let patch = capture_patch(&dir).unwrap();
    assert!(
        patch.contains("+++ b/.guvnor/runs/x/review.json"),
        "the destination must appear as a header, not as `rename to`: {patch}"
    );
    let e = validate_patch(&patch, "impl").unwrap_err().to_string();
    assert!(e.contains(".guvnor/runs/x/review.json"), "{e}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Verbatim `git diff --cached --binary --no-renames` after
/// `git mv src/keep.txt .guvnor/runs/x/review.json`. WITH rename detection
/// git emits only `similarity index` / `rename from` / `rename to` and no
/// `---`/`+++` pair at all, so the destination is invisible to every path
/// check and a lane can post its work into guvnor's own evidence directory.
#[test]
fn a_rename_into_a_denied_directory_is_caught() {
    let patch = "\
diff --git a/.guvnor/runs/x/review.json b/.guvnor/runs/x/review.json
new file mode 100644
index 0000000..e0808fa
--- /dev/null
+++ b/.guvnor/runs/x/review.json
@@ -0,0 +1 @@
+keep me
diff --git a/src/keep.txt b/src/keep.txt
deleted file mode 100644
index e0808fa..0000000
--- a/src/keep.txt
+++ /dev/null
@@ -1 +0,0 @@
-keep me
";
    // /dev/null is not a path, and both real ends are visible
    assert_eq!(
        patch_paths(patch),
        vec![".guvnor/runs/x/review.json".to_string(), "src/keep.txt".to_string()]
    );
    let e = validate_patch(patch, "impl").unwrap_err().to_string();
    assert!(e.contains(".guvnor/runs/x/review.json"), "{e}");
}

/// Verbatim git output for filenames containing a tab and a non-ASCII byte:
/// git wraps the whole `a/path` in quotes, which `strip_prefix("+++ b/")`
/// never matches. A path the checks cannot read is one they cannot fence,
/// and the `.claude/` write below is exactly what the fence is for.
#[test]
fn c_quoted_paths_are_read_not_skipped() {
    let patch = "\
diff --git \"a/.claude/set\\tting.json\" \"b/.claude/set\\tting.json\"
new file mode 100644
index 0000000..587be6b
--- /dev/null
+++ \"b/.claude/set\\tting.json\"
@@ -0,0 +1 @@
+x
diff --git \"a/caf\\303\\251.js\" \"b/caf\\303\\251.js\"
new file mode 100644
index 0000000..975fbec
--- /dev/null
+++ \"b/caf\\303\\251.js\"
@@ -0,0 +1 @@
+y
";
    assert_eq!(
        patch_paths(patch),
        vec![".claude/set\\tting.json".to_string(), "caf\\303\\251.js".to_string()],
        "quoted paths must survive to the checks, still escaped"
    );
    let e = validate_patch(patch, "tests").unwrap_err().to_string();
    assert!(e.contains(".claude/"), "the fence must still see it: {e}");
}

/// A header shape guvnor has no rule for must stop the patch, not be walked
/// past: silently skipping it is how a file reaches a commit without ever
/// appearing in the list the human approves.
#[test]
fn an_unreadable_header_refuses_the_patch() {
    let patch = "\
diff --git a/src/a.js b/src/a.js
--- src/a.js
+++ src/a.js
@@ -0,0 +1 @@
+x
";
    let e = validate_patch(patch, "impl").unwrap_err().to_string();
    assert!(e.contains("cannot read a path from"), "{e}");
}

/// Verbatim `git diff --cached` output for deleting the line
/// `-- a/.guvnor/runs/x/state.json` from a file: the `-` prefix makes it
/// `--- a/...`, which is a file header everywhere except inside a hunk.
/// Read as one, guvnor rejects its own lane's honest work.
#[test]
fn a_removed_line_is_not_a_file_header() {
    let patch = "diff --git a/fixture.txt b/fixture.txt\n\
                 index 21a87f2..2fa992c 100644\n\
                 --- a/fixture.txt\n\
                 +++ b/fixture.txt\n\
                 @@ -1,2 +1 @@\n \
                 keep\n\
                 --- a/.guvnor/runs/x/state.json\n";
    assert_eq!(patch_paths(patch), vec!["fixture.txt".to_string()]);
    validate_patch(patch, "tests").unwrap();
}

#[test]
fn ensure_wt_ignored_excludes_container_and_keeps_tree_clean() {
    let dir = std::env::temp_dir().join(format!("guvnor-wtignore-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    guvnor::git::init_test_repo(&dir);
    std::fs::write(dir.join("f"), "x").unwrap();
    git(&dir, &["add", "-A"]).unwrap();
    git(&dir, &["commit", "-qm", "init"]).unwrap();
    ensure_wt_ignored(&dir).unwrap();
    ensure_wt_ignored(&dir).unwrap(); // idempotent — no duplicate line
    let excl = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap();
    assert_eq!(excl.matches(".guvnor/wt/").count(), 1);
    // a worktree inside the ignored container leaves the main tree clean
    // (the property the stage clean-tree check depends on)
    let wt = wt_container(&dir).join("probe");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git(&dir, &["worktree", "add", "--detach", wt.to_str().unwrap(), "HEAD"]).unwrap();
    let status = git(&dir, &["status", "--porcelain"]).unwrap();
    assert!(status.trim().is_empty(), "tree not clean: {status:?}");
    git(&dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_wt_match_does_not_leak_into_a_nested_run_id() {
    assert!(is_run_wt("r1-tests", "r1"));
    assert!(is_run_wt("r1-impl", "r1"));
    assert!(is_run_wt("r1-verif", "r1"));
    // the footgun: a longer run id starting with this one must not match,
    // or cleaning `r1` would delete `r1-more`'s live worktrees
    assert!(!is_run_wt("r1-more-tests", "r1"));
    assert!(is_run_wt("r1-more-tests", "r1-more"));
    assert!(!is_run_wt("r1-tests", "r2"));
    assert!(!is_run_wt("r1-scratch", "r1"));
}

#[test]
fn finds_overlapping_paths() {
    let tests = "diff --git a/test/A.hs b/test/A.hs\n--- a/test/A.hs\n+++ b/test/A.hs\n@@\n+x\n";
    let impl_clean = "diff --git a/src/B.hs b/src/B.hs\n--- a/src/B.hs\n+++ b/src/B.hs\n@@\n+y\n";
    assert!(overlapping_paths(tests, impl_clean).is_empty());
    // the real failure: impl re-creates a file tests.patch already owns
    assert_eq!(overlapping_paths(tests, PATCH), vec!["test/A.hs".to_string()]);
}

#[test]
fn validates_patch_scope() {
    // whole-repo policy: a patch spanning test/ and src/ is fine
    assert!(validate_patch(PATCH, "tests").is_ok());
    // no work at all is still a failure
    assert!(validate_patch("", "tests").is_err());
    // guvnor's own control surfaces stay off-limits
    let evil = "diff --git a/.claude/settings.json b/.claude/settings.json\n--- a/.claude/settings.json\n+++ b/.claude/settings.json\n@@\n+{}\n";
    assert!(validate_patch(evil, "impl").is_err());
    let tamper = "diff --git a/.guvnor/runs/x/state.json b/.guvnor/runs/x/state.json\n--- a/.guvnor/runs/x/state.json\n+++ b/.guvnor/runs/x/state.json\n@@\n+{}\n";
    assert!(validate_patch(tamper, "impl").is_err());
}
