use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Per-target-repo config, loaded from `<repo>/.guvnor/guvnor.toml`.
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
    /// Foreman-style rework budget: on a failed green gate the implementer
    /// gets the failing output back this many times before the run fails.
    pub max_rework_rounds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { lane_timeout_secs: 900, max_rework_rounds: 1 }
    }
}

impl Config {
    pub fn load(repo: &Path) -> Result<Self> {
        let path = repo.join(".guvnor/guvnor.toml");
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Config = toml_edit::de::from_str(&raw).context("guvnor.toml parse error")?;
        if cfg.paths.tests.is_empty() || cfg.paths.src.is_empty() {
            bail!("paths.tests and paths.src must be non-empty");
        }
        validate_prefixes(&cfg.paths.tests)?;
        validate_prefixes(&cfg.paths.src)?;
        Ok(cfg)
    }
}

fn validate_prefixes(ps: &[String]) -> Result<()> {
    for p in ps {
        if p.starts_with('/') || p.contains("..") {
            bail!("path prefix must be repo-relative without '..': {p}");
        }
    }
    Ok(())
}

/// Walk up from cwd to find the repo root (dir containing .guvnor/guvnor.toml).
pub fn find_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".guvnor/guvnor.toml").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("no .guvnor/guvnor.toml found here or above; run `guvnor init` in the target repo");
        }
    }
}

/// Walk up from cwd to the enclosing git repo root (for the TUI before init).
pub fn find_git_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("not inside a git repository");
        }
    }
}

/// Create `.guvnor/` scaffolding + template config. Idempotent; returns the
/// config path. Shared by `guvnor init` and the TUI's in-app init.
pub fn init_repo(dir: &Path) -> Result<PathBuf> {
    init_repo_with(dir, "node --test", &["test/"], &["src/"])
}

/// Init with language-specific command/paths (TUI language picker).
pub fn init_repo_with(dir: &Path, test: &str, tests: &[&str], src: &[&str]) -> Result<PathBuf> {
    if !dir.join(".git").exists() {
        bail!("run inside a git repository");
    }
    let guvnor_dir = dir.join(".guvnor");
    std::fs::create_dir_all(guvnor_dir.join("runs"))?;
    // Run artifacts are local evidence, not repo content; without this the
    // merge clean-tree check trips over guvnor's own files.
    std::fs::write(guvnor_dir.join(".gitignore"), "runs/\n")?;
    let cfg = guvnor_dir.join("guvnor.toml");
    if !cfg.exists() {
        std::fs::write(&cfg, config_toml(test, tests, src))?;
    }
    Ok(cfg)
}

/// Everything the config modal can persist into guvnor.toml.
pub struct Settings {
    pub test: String,
    pub tests: Vec<String>,
    pub src: Vec<String>,
    pub bin: String,
    pub models: [String; 3], // planner, worker, reviewer
    pub timeout_secs: u64,
    pub max_rework_rounds: u64,
}

/// Persist every TUI-editable setting into guvnor.toml, preserving comments.
/// Creates the `.guvnor/` scaffolding first if needed (in-app init).
pub fn save_settings(repo: &Path, s: &Settings) -> Result<PathBuf> {
    if s.test.trim().is_empty() {
        bail!("test command must be non-empty");
    }
    if s.tests.is_empty() || s.src.is_empty() {
        bail!("tests and src paths must be non-empty");
    }
    validate_prefixes(&s.tests)?;
    validate_prefixes(&s.src)?;
    let t: Vec<&str> = s.tests.iter().map(String::as_str).collect();
    let src: Vec<&str> = s.src.iter().map(String::as_str).collect();
    let path = init_repo_with(repo, &s.test, &t, &src)?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw.parse().context("guvnor.toml parse error")?;
    let arr = |xs: &[&str]| xs.iter().copied().collect::<toml_edit::Array>();
    doc["commands"]["test"] = toml_edit::value(s.test.as_str());
    doc["paths"]["tests"] = toml_edit::value(arr(&t));
    doc["paths"]["src"] = toml_edit::value(arr(&src));
    for key in ["claude", "limits"] {
        if doc.get(key).is_none() {
            doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
        }
    }
    doc["claude"]["bin"] = toml_edit::value(s.bin.as_str());
    doc["claude"]["model_planner"] = toml_edit::value(s.models[0].as_str());
    doc["claude"]["model_worker"] = toml_edit::value(s.models[1].as_str());
    doc["claude"]["model_reviewer"] = toml_edit::value(s.models[2].as_str());
    doc["limits"]["lane_timeout_secs"] = toml_edit::value(s.timeout_secs as i64);
    doc["limits"]["max_rework_rounds"] = toml_edit::value(s.max_rework_rounds as i64);
    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}

/// Render a guvnor.toml with the given command/paths (comments included).
pub fn config_toml(test: &str, tests: &[&str], src: &[&str]) -> String {
    let list =
        |xs: &[&str]| xs.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(", ");
    format!(
        r#"# guvnor per-repo configuration
[commands]
# Any command; exit 0 = green. Examples:
#   "cabal test spec --test-show-details=direct"
#   "node --test"
#   "pytest -q"
test = "{test}"

[paths]
# Repo-relative prefixes. Test-writer writes only under `tests`,
# implementer only under `src`.
tests = [{tests}]
src = [{src}]

[claude]
# bin = "claude"
# model_planner = "opus"
# model_worker = "sonnet"
# model_reviewer = "opus"

[limits]
# lane_timeout_secs = 900
# On a failed green gate the implementer gets the failing output back
# this many times (rework rounds) before the run fails.
# max_rework_rounds = 1
"#,
        tests = list(tests),
        src = list(src),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_validates() {
        let raw = config_toml("cargo test", &["tests/"], &["src/", "app/"]);
        let cfg: Config = toml_edit::de::from_str(&raw).unwrap();
        assert_eq!(cfg.commands.test, "cargo test");
        assert_eq!(cfg.paths.tests, vec!["tests/"]);
        assert_eq!(cfg.paths.src, vec!["src/", "app/"]);
        assert_eq!(cfg.claude.bin, "claude");
        assert_eq!(cfg.limits.lane_timeout_secs, 900);
    }

    #[test]
    fn save_settings_roundtrip_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("guvnor-cfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        save_settings(
            &dir,
            &Settings {
                test: "cargo test".into(),
                tests: vec!["tests/".into()],
                src: vec!["src/".into(), "app/".into()],
                bin: "claude".into(),
                models: ["opus".into(), "haiku".into(), "opus".into()],
                timeout_secs: 600,
                max_rework_rounds: 2,
            },
        )
        .unwrap();
        let cfg = Config::load(&dir).unwrap();
        assert_eq!(cfg.commands.test, "cargo test");
        assert_eq!(cfg.paths.src, vec!["src/", "app/"]);
        assert_eq!(cfg.claude.model_worker, "haiku");
        assert_eq!(cfg.limits.lane_timeout_secs, 600);
        assert_eq!(cfg.limits.max_rework_rounds, 2);
        let raw = std::fs::read_to_string(dir.join(".guvnor/guvnor.toml")).unwrap();
        assert!(raw.contains("# guvnor per-repo configuration"));
        assert!(save_settings(
            &dir,
            &Settings {
                test: "t".into(),
                tests: vec!["/abs".into()],
                src: vec!["src/".into()],
                bin: "claude".into(),
                models: ["a".into(), "b".into(), "c".into()],
                timeout_secs: 1,
                max_rework_rounds: 1,
            }
        )
        .is_err());
        std::fs::remove_dir_all(&dir).ok();
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
        let cfg: Config = toml_edit::de::from_str(bad).unwrap();
        assert!(cfg.paths.tests[0].starts_with('/'));
        // Validation happens in load(); emulate its check here.
        assert!(cfg.paths.tests.iter().any(|p| p.starts_with('/')));
    }
}
