//! Deterministic guards invoked by Claude Code PreToolUse hooks.
//! Exit 0 = allow, exit 2 = block (message on stderr reaches the model).
//! Config travels in env vars embedded in the hook command string:
//!   GAFFER_ALLOW=test/,spec/  gaffer hook write
//! Guards are the backstop; lane prompts state the same constraints up front.

use anyhow::Result;
use serde_json::Value;
use std::io::Read;
use std::path::{Component, Path};

pub fn run_write_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let file_path = v["tool_input"]["file_path"]
        .as_str()
        .or_else(|| v["tool_input"]["notebook_path"].as_str())
        .unwrap_or("");
    let project_dir = std::env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| std::env::current_dir().unwrap().display().to_string());
    let allow: Vec<String> = std::env::var("GAFFER_ALLOW")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    match check_write(file_path, &project_dir, &allow) {
        Ok(()) => Ok(0),
        Err(msg) => {
            eprintln!("gaffer: BLOCKED write to {file_path}: {msg}");
            Ok(2)
        }
    }
}

pub fn run_bash_guard() -> Result<i32> {
    let input = read_stdin()?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let command = v["tool_input"]["command"].as_str().unwrap_or("");
    match check_bash(command) {
        Ok(()) => Ok(0),
        Err(msg) => {
            eprintln!("gaffer: BLOCKED bash command: {msg}");
            Ok(2)
        }
    }
}

/// Allow only paths inside the project dir that start with an allowed
/// repo-relative prefix. Rejects traversal and absolute escapes.
pub fn check_write(file_path: &str, project_dir: &str, allow: &[String]) -> Result<(), String> {
    if file_path.is_empty() {
        return Err("no file path in tool input".into());
    }
    let rel = relativize(file_path, project_dir)?;
    if Path::new(&rel).components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path traversal".into());
    }
    if allow.iter().any(|p| rel.starts_with(p.as_str())) {
        Ok(())
    } else {
        Err(format!("'{rel}' is outside allowed prefixes {allow:?}"))
    }
}

fn relativize(file_path: &str, project_dir: &str) -> Result<String, String> {
    let p = file_path.replace('\\', "/");
    if !p.starts_with('/') {
        return Ok(p);
    }
    let root = format!("{}/", project_dir.trim_end_matches('/'));
    p.strip_prefix(&root)
        .map(str::to_string)
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
    if words.iter().any(|w| *w == "git") {
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

    fn allow() -> Vec<String> {
        vec!["test/".to_string()]
    }

    #[test]
    fn write_inside_allowed_prefix_ok() {
        assert!(check_write("/wt/test/FooSpec.hs", "/wt", &allow()).is_ok());
        assert!(check_write("test/FooSpec.hs", "/wt", &allow()).is_ok());
    }

    #[test]
    fn write_outside_prefix_blocked() {
        assert!(check_write("/wt/src/Evil.hs", "/wt", &allow()).is_err());
        assert!(check_write("src/Evil.hs", "/wt", &allow()).is_err());
    }

    #[test]
    fn write_escape_blocked() {
        assert!(check_write("/etc/passwd", "/wt", &allow()).is_err());
        assert!(check_write("test/../src/Evil.hs", "/wt", &allow()).is_err());
        assert!(check_write("", "/wt", &allow()).is_err());
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
