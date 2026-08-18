use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Approval {
    pub approved: bool,
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub note: String,
    /// For the spec gate: sha256 of spec.json at approval time. Runs refuse
    /// a spec edited after approval — approvals bind to content, not files.
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Gates {
    pub spec: Approval,
    pub tests: Approval,
    pub work: Approval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub title: String,
    /// planned | spec_approved | red_ok | green_ok | reviewed | merged | failed:<why>
    pub status: String,
    pub gates: Gates,
    #[serde(default)]
    pub tests_patch_sha256: String,
    #[serde(default)]
    pub impl_patch_sha256: String,
    #[serde(default)]
    pub red_reason: String,
}

impl State {
    pub fn new(id: &str, title: &str) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: "planned".into(),
            gates: Gates::default(),
            tests_patch_sha256: String::new(),
            impl_patch_sha256: String::new(),
            red_reason: String::new(),
        }
    }

    pub fn load(run_dir: &Path) -> Result<Self> {
        let p = run_dir.join("state.json");
        let raw = std::fs::read_to_string(&p)
            .with_context(|| format!("cannot read {}", p.display()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, run_dir: &Path) -> Result<()> {
        std::fs::write(run_dir.join("state.json"), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

pub fn runs_root(repo: &Path) -> PathBuf {
    repo.join(".gaffer/runs")
}

/// Resolve a possibly-abbreviated run id to its directory.
pub fn resolve_run_dir(repo: &Path, id_prefix: &str) -> Result<PathBuf> {
    let root = runs_root(repo);
    let mut matches: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(id_prefix) && e.path().is_dir() {
                matches.push(e.path());
            }
        }
    }
    match matches.len() {
        0 => bail!("no run matches '{id_prefix}' under {}", root.display()),
        1 => Ok(matches.remove(0)),
        n => bail!("'{id_prefix}' is ambiguous ({n} matches)"),
    }
}

pub fn slugify(title: &str, max: usize) -> String {
    let mut s: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.truncate(max);
    s.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basics() {
        assert_eq!(slugify("Add add3 to Lib!", 24), "add-add3-to-lib");
        assert_eq!(slugify("  weird---spaces  ", 8), "weird-s");
    }

    #[test]
    fn state_roundtrip_and_resolve() {
        let dir = std::env::temp_dir().join(format!("gaffer-st-{}", std::process::id()));
        let run = dir.join(".gaffer/runs/123-abc");
        std::fs::create_dir_all(&run).unwrap();
        let st = State::new("123-abc", "t");
        st.save(&run).unwrap();
        let loaded = State::load(&run).unwrap();
        assert_eq!(loaded.status, "planned");
        let resolved = resolve_run_dir(&dir, "123").unwrap();
        assert!(resolved.ends_with("123-abc"));
        assert!(resolve_run_dir(&dir, "zzz").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
