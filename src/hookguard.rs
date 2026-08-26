//! Deterministic guards invoked by Claude Code PreToolUse hooks.
//! Exit 0 = allow, exit 2 = block (message on stderr reaches the model).
//! Guards are the backstop; lane prompts state the same constraints up front.

use anyhow::Result;
use serde_json::Value;
use std::io::Read;
use std::path::{Component, Path};

/// Paths no lane may ever write, however permissive the rest of the policy is.
/// `.claude/` holds the hook config that enforces containment — a lane that
/// could rewrite it could disable its own guard and then escape the repo.
/// `.guvnor/` holds the config and the run evidence (patches, digests,
/// verdicts); evidence a lane can edit is not evidence.
const DENIED: &[&str] = &[".claude/", ".guvnor/"];

/// The denied surface this repo-relative path falls under, if any. Matches the
/// first path component, case-folded: the bare directory and any case variant
/// name the same inode on a case-insensitive filesystem (macOS by default).
pub fn denied_prefix(rel: &str) -> Option<&'static str> {
    let first = Path::new(rel).components().next()?;
    let first = first.as_os_str().to_string_lossy().to_lowercase();
    DENIED.iter().copied().find(|d| d.trim_end_matches('/') == first)
}

pub fn run_write_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let file_path = v["tool_input"]["file_path"]
        .as_str()
        .or_else(|| v["tool_input"]["notebook_path"].as_str())
        .unwrap_or("");
    let project_dir = project_dir()?;
    // Per-run deny list: exact repo-relative paths an earlier lane already owns.
    // Read from `.claude/deny` (written by `lane::write_settings`), NUL-joined —
    // the one byte no POSIX path can ever contain, so it needs no escaping.
    let deny_raw = std::fs::read_to_string(Path::new(&project_dir).join(".claude/deny")).unwrap_or_default();
    let deny: Vec<String> =
        deny_raw.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
    match check_write(file_path, &project_dir, &deny) {
        Ok(()) => Ok(0),
        Err(msg) => {
            eprintln!("guvnor: BLOCKED write to {file_path}: {msg}");
            Ok(2)
        }
    }
}

/// Reads are fenced to the worktree too. Three reasons, in order of severity:
/// the main repo's `.guvnor/runs/<id>/tests.patch` is readable by absolute path
/// from an implementer worktree (a decorrelation hole); a lane's own
/// `.claude/deny` file names the test files it must not see; and a read of
/// `~/Documents` or similar makes the OS raise a consent dialog that headless
/// `claude -p` cannot answer, so the lane hangs until timeout.
pub fn run_read_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let ti = &v["tool_input"];
    // Read uses file_path; Glob/Grep use an optional path (empty = cwd)
    let path = ti["file_path"].as_str().or_else(|| ti["path"].as_str()).unwrap_or("");
    let project_dir = project_dir()?;
    match check_read(path, &project_dir) {
        Ok(()) => Ok(0),
        Err(msg) => {
            eprintln!("guvnor: BLOCKED read of {path}: {msg}");
            Ok(2)
        }
    }
}

/// Allow anything inside the worktree except guvnor's own control surfaces.
/// An empty path means "cwd", which is the worktree by construction.
pub fn check_read(path: &str, project_dir: &str) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    let rel = relativize(path, project_dir)?;
    if let Some(d) = denied_prefix(&rel) {
        return Err(format!("'{rel}' is guvnor's own control surface ({d})"));
    }
    Ok(())
}

pub fn run_bash_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let command = v["tool_input"]["command"].as_str().unwrap_or("");
    match check_bash(command) {
        Ok(()) => Ok(0),
        Err(msg) => {
            eprintln!("guvnor: BLOCKED bash command: {msg}");
            Ok(2)
        }
    }
}

/// Allow any path inside the project dir except guvnor's own control surfaces
/// and `deny` (exact repo-relative paths an earlier lane already owns — writing
/// them would make the two patches non-composable). Rejects traversal and
/// absolute escapes: a lane cannot write outside the repo it was started in.
pub fn check_write(file_path: &str, project_dir: &str, deny: &[String]) -> Result<(), String> {
    if file_path.is_empty() {
        return Err("no file path in tool input".into());
    }
    let rel = relativize(file_path, project_dir)?;
    if let Some(d) = denied_prefix(&rel) {
        return Err(format!("'{rel}' is guvnor's own control surface ({d})"));
    }
    if deny.iter().any(|d| d == &rel) {
        return Err(format!(
            "'{rel}' is owned by an independent lane's patch — you must not create or edit it"
        ));
    }
    Ok(())
}

/// One repo-relative form for a path that may arrive absolute, `./`-prefixed,
/// or with redundant separators. Errs on anything that leaves the project dir.
/// `Path::components` does the normalising, so `.` and `//` collapse and a `..`
/// survives as a `ParentDir` to reject rather than as text to pattern-match.
fn relativize(file_path: &str, project_dir: &str) -> Result<String, String> {
    let p = Path::new(file_path);
    let rel = if p.is_absolute() {
        p.strip_prefix(project_dir)
            .map_err(|_| format!("absolute path outside project dir {project_dir}"))?
    } else {
        p
    };
    let mut out = std::path::PathBuf::new();
    for c in rel.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => out.push(s),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("path traversal".into())
            }
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Block history-mutating git regardless of flags/subcommand position, plus any
/// command that names a control surface, steps outside the worktree, or
/// re-enters guvnor. A shell has no per-path tool input to guard, so the check
/// is on the raw text and refuses wholesale: `cat ../<sibling>-tests/test/x`
/// reads around `check_read`, `rm -rf ../../runs/<id>` deletes the evidence,
/// and `guvnor approve` would let a lane hold its own gate. It over-blocks
/// (`ls ..`) on purpose. A lane has no business outside the tree it was handed.
pub fn check_bash(command: &str) -> Result<(), String> {
    let words: Vec<&str> = command
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    const BANNED: &[&str] =
        &["commit", "push", "reset", "rebase", "merge", "tag", "worktree", "cherry-pick", "am"];
    // Case-insensitive: PATH lookup on a case-insensitive filesystem resolves
    // `GIT` to the same binary.
    if words.iter().any(|w| w.eq_ignore_ascii_case("git")) {
        if let Some(bad) = words.iter().find(|w| BANNED.contains(&w.to_lowercase().as_str())) {
            return Err(format!("git {bad} is forbidden in lanes; report instead"));
        }
    }
    for pat in [".guvnor", ".claude", ".."] {
        if command.contains(pat) {
            return Err(format!("'{pat}' is out of bounds for a lane shell"));
        }
    }
    if words.iter().any(|w| w.eq_ignore_ascii_case("guvnor")) {
        return Err("guvnor's verbs are the human's; a lane may not hold its own gate".into());
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

/// The worktree the lane runs in. Claude Code sets `CLAUDE_PROJECT_DIR`; cwd is
/// the fallback. Fallible rather than `unwrap`, because a panicking guard exits
/// 101 and Claude Code lets the tool call through on anything but exit 2.
fn project_dir() -> Result<String> {
    match std::env::var("CLAUDE_PROJECT_DIR") {
        Ok(d) => Ok(d),
        Err(_) => Ok(std::env::current_dir()?.display().to_string()),
    }
}
