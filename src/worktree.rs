use crate::git::git;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Lane worktrees live under `.guvnor/wt/` — inside the repo dir but kept out
/// of the tracked tree via `.git/info/exclude` (see `ensure_wt_ignored`), so
/// nothing is created outside the repo. A test runner in one lane never picks
/// up sibling fixtures: each lane runs tests with cwd = its OWN worktree,
/// never an ancestor of another, and a worktree checkout has no nested `wt/`
/// (it's git-excluded, so never in HEAD).
fn wt_container(repo: &Path) -> PathBuf {
    repo.join(".guvnor/wt")
}

/// Exclude the worktree container from git locally. It's per-developer
/// throwaway state; a *tracked* .gitignore edit would dirty the tree and trip
/// the stage clean-tree check, so the exclusion goes in
/// `$GIT_DIR/info/exclude` instead (local, uncommitted). Idempotent — safe to
/// call every run.
pub fn ensure_wt_ignored(repo: &Path) -> Result<()> {
    const ENTRY: &str = ".guvnor/wt/";
    let rel = git(repo, &["rev-parse", "--git-path", "info/exclude"])?;
    let path = repo.join(rel.trim());
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == ENTRY) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(ENTRY);
    content.push('\n');
    std::fs::write(&path, content)?;
    Ok(())
}

pub fn create(repo: &Path, run_id: &str, lane: &str) -> Result<PathBuf> {
    let dir = wt_container(repo).join(format!("{run_id}-{lane}"));
    if dir.exists() {
        remove(repo, &dir)?;
    }
    std::fs::create_dir_all(dir.parent().unwrap())?;
    git(repo, &["worktree", "add", "--detach", dir.to_str().unwrap(), "HEAD"])
        .context("git worktree add failed")?;
    Ok(dir)
}

pub fn remove(repo: &Path, dir: &Path) -> Result<()> {
    if dir.exists() {
        // --force: throwaway trees are dirty by design.
        let _ = git(repo, &["worktree", "remove", "--force", dir.to_str().unwrap()]);
        if dir.exists() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

/// The lanes that own a worktree. Also the closed set `remove_run` matches on:
/// a bare `<run_id>-` prefix would also match a longer run id that starts with
/// this one (same-second ids whose slugs nest, e.g. `add` and `add-more`) and
/// delete another run's live tree.
const LANES: [&str; 3] = ["tests", "impl", "verif"];

/// Is `name` a worktree dir belonging to `run_id`?
fn is_run_wt(name: &str, run_id: &str) -> bool {
    name.strip_prefix(run_id)
        .and_then(|r| r.strip_prefix('-'))
        .is_some_and(|lane| LANES.contains(&lane))
}

/// Remove every lane worktree belonging to a run. Used on success so callers
/// don't have to track which trees they created — a fix round creates a
/// different set than the initial run.
pub fn remove_run(repo: &Path, run_id: &str) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(wt_container(repo)) {
        for e in entries.flatten() {
            if is_run_wt(&e.file_name().to_string_lossy(), run_id) {
                remove(repo, &e.path())?;
            }
        }
    }
    Ok(())
}

/// Reset a worktree to pristine HEAD (rework rounds re-apply patches from
/// scratch: cumulative patches don't stack on an already-patched tree).
pub fn reset_hard(wt: &Path) -> Result<()> {
    git(wt, &["reset", "--hard", "HEAD"])?;
    // -e .claude: keep guvnor's own hook scaffolding written by write_settings
    git(wt, &["clean", "-fd", "-e", ".claude"])?;
    Ok(())
}

/// Capture everything a lane did as one patch (staged snapshot of the
/// worktree, including new files). Excludes .claude/ — that's guvnor's own
/// hook scaffolding, not lane work.
pub fn capture_patch(wt: &Path) -> Result<String> {
    git(wt, &["add", "-A", "--", ".", ":(exclude).claude"])?;
    git(wt, &["diff", "--cached", "--binary"])
}

pub fn apply_patch(wt: &Path, patch: &str) -> Result<()> {
    apply_args(wt, patch, &["apply", "--whitespace=nowarn"])
}

/// Apply to index+tree in the MAIN repo (the stage step): leaves it staged.
pub fn apply_patch_staged(repo: &Path, patch: &str) -> Result<()> {
    apply_args(repo, patch, &["apply", "--index", "--whitespace=nowarn"])
}

/// Take it back out again (`unstage`). `-R` also removes files the patch
/// created, from the index and from disk — verified, so there is no `git rm`
/// half to forget.
pub fn reverse_patch_staged(repo: &Path, patch: &str) -> Result<()> {
    apply_args(repo, patch, &["apply", "-R", "--index", "--whitespace=nowarn"])
}

fn apply_args(dir: &Path, patch: &str, args: &[&str]) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("git apply spawn")?;
    child.stdin.as_mut().unwrap().write_all(patch.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git apply failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// A patch's lines, each tagged with whether it is hunk content rather than a
/// header. `--- ` and `+++ ` are header syntax in header position only: delete
/// a line reading `-- | mean` and the patch carries `--- | mean`, which any
/// scan that ignores the hunk boundary reads as a file header.
pub fn patch_lines(patch: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut in_hunk = false;
    patch.lines().map(move |l| {
        if l.starts_with("diff --git ") {
            in_hunk = false;
        } else if l.starts_with("@@") {
            in_hunk = true;
        }
        (l, in_hunk)
    })
}

/// Paths touched by a patch, read from the `---`/`+++` lines: each is a fixed
/// prefix then the path to end of line, so nothing in the path itself (a
/// space, or literally " b/") can be mistaken for the header's own syntax the
/// way splitting the `diff --git` summary line would.
pub fn patch_paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for (line, in_hunk) in patch_lines(patch) {
        if in_hunk {
            continue;
        }
        let path = line.strip_prefix("--- a/").or_else(|| line.strip_prefix("+++ b/"));
        if let Some(p) = path {
            if !paths.iter().any(|x| x == p) {
                paths.push(p.to_string());
            }
        }
    }
    paths
}

/// Paths that both patches touch. Applying both to one tree would collide
/// (`already exists in working directory`), so an overlap must be caught as a
/// gate failure before `git apply` turns it into a raw error.
pub fn overlapping_paths(first: &str, second: &str) -> Vec<String> {
    let a = patch_paths(first);
    patch_paths(second).into_iter().filter(|p| a.contains(p)).collect()
}

/// Server-side re-validation of a lane's patch. The hook is the first line of
/// defence; this is the backstop that doesn't trust the lane's environment at
/// all. Lanes may touch the whole repo, so all this enforces is: there IS work,
/// and it stays off guvnor's own control surfaces.
pub fn validate_patch(patch: &str, label: &str) -> Result<()> {
    let paths = patch_paths(patch);
    if paths.is_empty() {
        bail!("{label} patch is empty — lane produced no work");
    }
    for p in &paths {
        if let Some(d) = crate::hookguard::denied_prefix(p) {
            bail!("{label} patch touches guvnor's control surface '{p}' ({d})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/test/A.hs b/test/A.hs\nnew file mode 100644\n--- /dev/null\n+++ b/test/A.hs\n@@\n+x\ndiff --git a/src/B.hs b/src/B.hs\n--- a/src/B.hs\n+++ b/src/B.hs\n@@\n+y\n";

    #[test]
    fn extracts_unique_paths() {
        assert_eq!(patch_paths(PATCH), vec!["test/A.hs".to_string(), "src/B.hs".to_string()]);
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
        git(&dir, &["init", "-q"]).unwrap();
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
}
