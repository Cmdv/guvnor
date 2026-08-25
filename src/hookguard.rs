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

/// The first denied prefix this repo-relative path falls under, if any.
pub fn denied_prefix(rel: &str) -> Option<&'static str> {
    DENIED.iter().copied().find(|d| rel.starts_with(d))
}

pub fn run_write_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let file_path = v["tool_input"]["file_path"]
        .as_str()
        .or_else(|| v["tool_input"]["notebook_path"].as_str())
        .unwrap_or("");
    let project_dir = std::env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| std::env::current_dir().unwrap().display().to_string());
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
    let project_dir = std::env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| std::env::current_dir().unwrap().display().to_string());
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
    if Path::new(&rel).components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path traversal".into());
    }
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
    if Path::new(&rel).components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path traversal".into());
    }
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

fn relativize(file_path: &str, project_dir: &str) -> Result<String, String> {
    let p = file_path.replace('\\', "/");
    if !p.starts_with('/') {
        // "./x" and "x" name the same file; compare in one form
        return Ok(p.trim_start_matches("./").to_string());
    }
    let root = format!("{}/", project_dir.trim_end_matches('/'));
    p.strip_prefix(&root)
        .map(|r| r.trim_start_matches("./").to_string())
        .ok_or_else(|| format!("absolute path outside project dir {project_dir}"))
}

/// Block history-mutating git regardless of flags/subcommand position.
pub fn check_bash(command: &str) -> Result<(), String> {
    let words: Vec<&str> = command
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    const BANNED: &[&str] =
        &["commit", "push", "reset", "rebase", "merge", "tag", "worktree", "cherry-pick", "am"];
    if words.contains(&"git") {
        if let Some(bad) = words.iter().find(|w| BANNED.contains(&w.to_lowercase().as_str())) {
            return Err(format!("git {bad} is forbidden in lanes; report instead"));
        }
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_deny() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn write_anywhere_inside_repo_ok() {
        // policy: the whole repo is writable, not just test/ or src/
        assert!(check_write("/wt/test/FooSpec.hs", "/wt", &no_deny()).is_ok());
        assert!(check_write("test/FooSpec.hs", "/wt", &no_deny()).is_ok());
        assert!(check_write("/wt/src/Lib.hs", "/wt", &no_deny()).is_ok());
        assert!(check_write("package.json", "/wt", &no_deny()).is_ok()); // root scaffolding
        assert!(check_write("/wt/LICENSE", "/wt", &no_deny()).is_ok());
        assert!(check_write("deep/nested/dir/file.js", "/wt", &no_deny()).is_ok());
    }

    #[test]
    fn write_to_guvnor_control_surfaces_blocked() {
        // a lane rewriting the hook config could disable its own containment
        assert!(check_write("/wt/.claude/settings.json", "/wt", &no_deny()).is_err());
        assert!(check_write(".claude/settings.json", "/wt", &no_deny()).is_err());
        // run evidence a lane can edit is not evidence
        assert!(check_write("/wt/.guvnor/guvnor.toml", "/wt", &no_deny()).is_err());
        assert!(check_write(".guvnor/runs/x/state.json", "/wt", &no_deny()).is_err());
    }

    #[test]
    fn write_to_another_lanes_paths_blocked() {
        // the implementer must not create files tests.patch already owns —
        // otherwise the two patches can't both apply to the verif tree
        let owned = vec!["test/a.test.js".to_string()];
        assert!(check_write("/wt/test/a.test.js", "/wt", &owned).is_err());
        assert!(check_write("test/a.test.js", "/wt", &owned).is_err());
        assert!(check_write("./test/a.test.js", "/wt", &owned).is_err()); // ./ normalized
        // a different file under the same dir is fine (no collision)
        assert!(check_write("test/b.test.js", "/wt", &owned).is_ok());
        assert!(check_write("src/a.js", "/wt", &owned).is_ok());
    }

    #[test]
    fn write_escape_blocked() {
        assert!(check_write("/etc/passwd", "/wt", &no_deny()).is_err());
        assert!(check_write("test/../../src/Evil.hs", "/wt", &no_deny()).is_err());
        assert!(check_write("", "/wt", &no_deny()).is_err());
    }

    #[test]
    fn reads_are_fenced_to_the_worktree() {
        assert!(check_read("src/a.js", "/wt").is_ok());
        assert!(check_read("/wt/src/a.js", "/wt").is_ok());
        assert!(check_read("", "/wt").is_ok()); // Glob/Grep with no path = cwd
        // the decorrelation hole: the real repo's evidence, by absolute path
        assert!(check_read("/repo/.guvnor/runs/x/tests.patch", "/wt").is_err());
        assert!(check_read(".guvnor/runs/x/tests.patch", "/wt").is_err());
        // the lane's own .claude/deny file names the test files
        assert!(check_read(".claude/settings.json", "/wt").is_err());
        assert!(check_read(".claude/deny", "/wt").is_err());
        // outside the worktree at all — this is what makes the OS prompt
        assert!(check_read("/Users/me/Documents/x", "/wt").is_err());
        assert!(check_read("src/../../secrets", "/wt").is_err());
    }

    #[test]
    fn bash_git_mutations_blocked_others_allowed() {
        assert!(check_bash("git commit -m x").is_err());
        assert!(check_bash("git -C /tmp push origin").is_err());
        assert!(check_bash("cd a && git rebase main").is_err());
        assert!(check_bash("git status && git diff").is_ok());
        assert!(check_bash("cabal test spec").is_ok());
        // 'commit' outside a git command is fine
        assert!(check_bash("echo commit").is_ok());
    }
}
