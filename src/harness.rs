use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub struct TestOutcome {
    pub green: bool,
    pub exit_code: Option<i32>,
    /// Last lines of combined output — the evidence a human sees in the case file.
    pub tail: String,
}

/// Run the configured test command via `sh -c` in a worktree.
pub fn run_tests(dir: &Path, test_cmd: &str) -> Result<TestOutcome> {
    let out = Command::new("sh")
        .args(["-c", test_cmd])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to spawn: sh -c '{test_cmd}'"))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(TestOutcome {
        green: out.status.success(),
        exit_code: out.status.code(),
        tail: crate::lane::tail(&combined, 40),
    })
}
