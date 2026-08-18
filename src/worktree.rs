use crate::digest::git;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Lane worktrees live OUTSIDE the repo (sibling dir) so test runners with
/// auto-discovery never see them — spike finding: `node --test` picked up
/// fixture files inside the tree.
pub fn wt_container(repo: &Path) -> PathBuf {
    let name = repo.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    repo.parent().unwrap_or(repo).join(format!("{name}-gaffer-wt"))
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

/// Capture everything a lane did as one patch (staged snapshot of the
/// worktree, including new files). Excludes .claude/ — that's gaffer's own
/// hook scaffolding, not lane work.
pub fn capture_patch(wt: &Path) -> Result<String> {
    git(wt, &["add", "-A", "--", ".", ":(exclude).claude"])?;
    git(wt, &["diff", "--cached", "--binary"])
}

pub fn apply_patch(wt: &Path, patch: &str) -> Result<()> {
    apply_args(wt, patch, &["apply", "--whitespace=nowarn"])
}

/// Apply to index+tree in the MAIN repo (merge step): leaves changes staged.
pub fn apply_patch_staged(repo: &Path, patch: &str) -> Result<()> {
    apply_args(repo, patch, &["apply", "--index", "--whitespace=nowarn"])
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

/// Paths touched by a patch, from `diff --git a/X b/Y` headers.
/// ponytail: assumes no spaces/quotes in repo paths — fine for v1.
pub fn patch_paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some((a, b)) = rest.split_once(" b/") {
                for p in [a, b] {
                    if !p.is_empty() && !paths.contains(&p.to_string()) {
                        paths.push(p.to_string());
                    }
                }
            }
        }
    }
    paths
}

pub fn validate_patch_within(patch: &str, prefixes: &[String], label: &str) -> Result<()> {
    let paths = patch_paths(patch);
    if paths.is_empty() {
        bail!("{label} patch is empty — lane produced no work");
    }
    for p in &paths {
        if !prefixes.iter().any(|pre| p.starts_with(pre.as_str())) {
            bail!("{label} patch touches forbidden path '{p}' (allowed: {prefixes:?})");
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

    #[test]
    fn validates_prefixes() {
        let tests_only = vec!["test/".to_string()];
        assert!(validate_patch_within(PATCH, &tests_only, "tests").is_err());
        let both = vec!["test/".to_string(), "src/".to_string()];
        assert!(validate_patch_within(PATCH, &both, "tests").is_ok());
        assert!(validate_patch_within("", &both, "tests").is_err());
    }
}
