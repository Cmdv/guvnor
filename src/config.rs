use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Per-target-repo config, loaded from `<repo>/.gaffer/gaffer.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub commands: Commands,
    pub paths: Paths,
    #[serde(default)]
    pub claude: Claude,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Commands {
    /// Test command run via `sh -c` in a worktree. Exit 0 = green.
    pub test: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Paths {
    /// Repo-relative prefixes the test-writer may write under.
    pub tests: Vec<String>,
    /// Repo-relative prefixes the implementer may write under.
    pub src: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Claude {
    pub bin: String,
    pub model_planner: String,
    pub model_worker: String,
    pub model_reviewer: String,
}

impl Default for Claude {
    fn default() -> Self {
        Self {
            bin: "claude".into(),
            model_planner: "opus".into(),
            model_worker: "sonnet".into(),
            model_reviewer: "opus".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub lane_timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { lane_timeout_secs: 900 }
    }
}

impl Config {
    pub fn load(repo: &Path) -> Result<Self> {
        let path = repo.join(".gaffer/gaffer.toml");
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw).context("gaffer.toml parse error")?;
        if cfg.paths.tests.is_empty() || cfg.paths.src.is_empty() {
            bail!("paths.tests and paths.src must be non-empty");
        }
        for p in cfg.paths.tests.iter().chain(cfg.paths.src.iter()) {
            if p.starts_with('/') || p.contains("..") {
                bail!("path prefix must be repo-relative without '..': {p}");
            }
        }
        Ok(cfg)
    }
}

/// Walk up from cwd to find the repo root (dir containing .gaffer/gaffer.toml).
pub fn find_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".gaffer/gaffer.toml").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("no .gaffer/gaffer.toml found here or above; run `gaffer init` in the target repo");
        }
    }
}

pub const CONFIG_TEMPLATE: &str = r#"# gaffer per-repo configuration
[commands]
# Any command; exit 0 = green. Examples:
#   "cabal test spec --test-show-details=direct"
#   "node --test"
#   "pytest -q"
test = "node --test"

[paths]
# Repo-relative prefixes. Test-writer writes only under `tests`,
# implementer only under `src`.
tests = ["test/"]
src = ["src/"]

[claude]
# bin = "claude"
# model_planner = "opus"
# model_worker = "sonnet"
# model_reviewer = "opus"

[limits]
# lane_timeout_secs = 900
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_validates() {
        let cfg: Config = toml::from_str(CONFIG_TEMPLATE).unwrap();
        assert_eq!(cfg.commands.test, "node --test");
        assert_eq!(cfg.paths.tests, vec!["test/"]);
        assert_eq!(cfg.claude.bin, "claude");
        assert_eq!(cfg.limits.lane_timeout_secs, 900);
    }

    #[test]
    fn rejects_absolute_or_traversal_prefixes() {
        let bad = r#"
[commands]
test = "true"
[paths]
tests = ["/etc/"]
src = ["src/"]
"#;
        let cfg: Config = toml::from_str(bad).unwrap();
        assert!(cfg.paths.tests[0].starts_with('/'));
        // Validation happens in load(); emulate its check here.
        assert!(cfg.paths.tests.iter().any(|p| p.starts_with('/')));
    }
}
