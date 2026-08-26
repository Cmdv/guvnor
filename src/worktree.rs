use crate::git::{git, git_bytes};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// Lane worktrees live under `.guvnor/wt/` — inside the repo dir but kept out
/// of the tracked tree via `.git/info/exclude` (see `ensure_wt_ignored`), so
/// nothing is created outside the repo. A test runner in one lane never picks
/// up sibling fixtures: each lane runs tests with cwd = its OWN worktree,
/// never an ancestor of another, and a worktree checkout has no nested `wt/`
/// (it's git-excluded, so never in HEAD).
pub fn wt_container(repo: &Path) -> PathBuf {
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

/// git takes paths as command arguments, so a repo path that is not UTF-8 is a
/// condition to name rather than to panic on: `create` runs on the TUI's engine
/// thread, where a panic surfaces only as a failed job.
fn path_arg(p: &Path) -> Result<&str> {
    p.to_str().with_context(|| format!("path is not valid UTF-8: {}", p.display()))
}

pub fn create(repo: &Path, run_id: &str, lane: &str) -> Result<PathBuf> {
    let dir = wt_container(repo).join(format!("{run_id}-{lane}"));
    if dir.exists() {
        remove(repo, &dir);
    }
    let parent = dir.parent().context("worktree container has no parent")?;
    std::fs::create_dir_all(parent)?;
    git(repo, &["worktree", "add", "--detach", path_arg(&dir)?, "HEAD"])
        .context("git worktree add failed")?;
    Ok(dir)
}

/// Best effort by design: a throwaway tree that resists one route is taken out
/// by the next, and a leftover directory is not worth failing a run over. No
/// `Result`, because it never had a failure to report.
pub fn remove(repo: &Path, dir: &Path) {
    if dir.exists() {
        // --force: throwaway trees are dirty by design.
        let _ = git(repo, &["worktree", "remove", "--force", &dir.to_string_lossy()]);
        if dir.exists() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
    let _ = git(repo, &["worktree", "prune"]);
}

/// The lanes that own a worktree. Also the closed set `remove_run` matches on:
/// a bare `<run_id>-` prefix would also match a longer run id that starts with
/// this one (same-second ids whose slugs nest, e.g. `add` and `add-more`) and
/// delete another run's live tree.
const LANES: [&str; 3] = ["tests", "impl", "verif"];

/// Is `name` a worktree dir belonging to `run_id`?
pub fn is_run_wt(name: &str, run_id: &str) -> bool {
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
                remove(repo, &e.path());
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
///
/// `--no-renames` because a 100%-similarity rename carries no `---`/`+++` pair,
/// only `rename from`/`rename to`, so the path checks would never see where the
/// file went. As delete+add both ends are visible.
/// `core.quotePath=true` (git's default, pinned here) C-escapes non-ASCII path
/// bytes, so a filename can never be what makes a patch non-UTF-8.
pub fn capture_patch(wt: &Path) -> Result<String> {
    git(wt, &["add", "-A", "--", ".", ":(exclude).claude"])?;
    let out = git_bytes(
        wt,
        &["-c", "core.quotePath=true", "diff", "--cached", "--binary", "--no-renames"],
    )?;
    // ponytail: patches are Strings from here to disk, digest and `git apply`
    // stdin. A file whose CONTENT is not UTF-8 and has no NUL is diffed as text,
    // so refuse it loudly rather than let a lossy decode corrupt the evidence.
    // Upgrade path: carry Vec<u8> through capture -> disk -> apply.
    String::from_utf8(out).map_err(|e| {
        anyhow!(
            "patch is not valid UTF-8 (first bad byte at {}); guvnor cannot digest it honestly",
            e.utf8_error().valid_up_to()
        )
    })
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
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("git apply spawn")?;
    // Drain stderr on its own thread: git apply writes a line per rejected hunk,
    // so a large bad patch fills the pipe and both ends block waiting.
    let mut pipe = child.stderr.take().context("git apply stderr")?;
    let drain = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe.read_to_string(&mut s);
        s
    });
    // Dropping stdin here is what gives git its EOF.
    let wrote = child
        .stdin
        .take()
        .context("git apply stdin")
        .and_then(|mut w| w.write_all(patch.as_bytes()).context("writing patch to git apply"));
    let status = child.wait()?;
    let stderr = drain.join().unwrap_or_default();
    // Status first: a write that failed means git had already exited, and its own
    // message says why far better than "broken pipe" does.
    if !status.success() {
        bail!("git apply failed: {}", stderr.trim());
    }
    wrote?;
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

/// What a `---`/`+++` header line names.
enum Header<'a> {
    /// A path, with git's `a/`/`b/` prefix stripped. Still C-escaped if git
    /// escaped it, which leaves `.claude/` and `.guvnor/` literal, so the
    /// `denied_prefix` check still reads them.
    Path(&'a str),
    /// `/dev/null`: the absent side of a file being created or deleted.
    DevNull,
    /// A header whose path could not be read. A path the checks cannot see is a
    /// path they cannot fence, so callers must refuse rather than skip it.
    Unreadable,
}

/// Classify one line. `None` means it is not a `---`/`+++` header at all.
/// The path runs to end of line, so nothing inside it (a space, or literally
/// " b/") can be mistaken for the header's own syntax the way splitting the
/// `diff --git` summary line would.
fn header(line: &str) -> Option<Header<'_>> {
    let rest = line.strip_prefix("--- ").or_else(|| line.strip_prefix("+++ "))?;
    if rest == "/dev/null" {
        return Some(Header::DevNull);
    }
    // git wraps the whole `a/path` in quotes when the path needs escaping:
    // `--- "a/src\ttab.js"`.
    let inner = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(rest);
    match inner.strip_prefix("a/").or_else(|| inner.strip_prefix("b/")) {
        Some(p) => Some(Header::Path(p)),
        None => Some(Header::Unreadable),
    }
}

/// Paths touched by a patch, plus the first header that defied parsing.
/// Separated so `patch_paths` stays infallible for the file lists a human
/// reads, while `validate_patch` can refuse a patch it cannot fully see.
fn scan_paths(patch: &str) -> (Vec<String>, Option<String>) {
    let mut paths: Vec<String> = Vec::new();
    let mut bad = None;
    for (line, in_hunk) in patch_lines(patch) {
        if in_hunk {
            continue;
        }
        match header(line) {
            None | Some(Header::DevNull) => {}
            Some(Header::Path(p)) => {
                if !paths.iter().any(|x| x == p) {
                    paths.push(p.to_string());
                }
            }
            Some(Header::Unreadable) => bad = bad.or_else(|| Some(line.to_string())),
        }
    }
    (paths, bad)
}

pub fn patch_paths(patch: &str) -> Vec<String> {
    scan_paths(patch).0
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
    let (paths, unreadable) = scan_paths(patch);
    if let Some(line) = unreadable {
        bail!("{label} patch has a header guvnor cannot read a path from: {line}");
    }
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

