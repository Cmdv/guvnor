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

/// The three human approval gates. `clap::ValueEnum` gives `--gate spec|tests|work`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Gate {
    Spec,
    Tests,
    Work,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Spec => "spec",
            Gate::Tests => "tests",
            Gate::Work => "work",
        }
    }
}

impl Gates {
    pub fn slot_mut(&mut self, gate: Gate) -> &mut Approval {
        match gate {
            Gate::Spec => &mut self.spec,
            Gate::Tests => &mut self.tests,
            Gate::Work => &mut self.work,
        }
    }
}

/// Run lifecycle status. Serialized as a flat string (backward-compatible with
/// existing run dirs): the progress states plus `failed:<why>` for any terminal
/// failure, where `<why>` is a machine reason like `vacuous_baseline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Status {
    Planned,
    SpecApproved,
    RedOk,
    GreenOk,
    Reviewed,
    /// Patches applied to the main repo's index but not committed.
    Staged,
    /// Committed in the main repo. Guv'nor never pushes — that stays yours.
    Committed,
    Failed(String),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Status::Planned => f.write_str("planned"),
            Status::SpecApproved => f.write_str("spec_approved"),
            Status::RedOk => f.write_str("red_ok"),
            Status::GreenOk => f.write_str("green_ok"),
            Status::Reviewed => f.write_str("reviewed"),
            Status::Staged => f.write_str("staged"),
            Status::Committed => f.write_str("committed"),
            Status::Failed(why) => write!(f, "failed:{why}"),
        }
    }
}

impl From<String> for Status {
    fn from(s: String) -> Self {
        match s.as_str() {
            "planned" => Status::Planned,
            "spec_approved" => Status::SpecApproved,
            "red_ok" => Status::RedOk,
            "green_ok" => Status::GreenOk,
            "reviewed" => Status::Reviewed,
            "staged" => Status::Staged,
            "committed" => Status::Committed,
            // Runs from before staging and committing were told apart. They
            // were staged and never committed by guvnor, so that is what they
            // were: keep reading them rather than calling them failures.
            "merged" => Status::Staged,
            other => Status::Failed(other.strip_prefix("failed:").unwrap_or(other).to_string()),
        }
    }
}

impl From<Status> for String {
    fn from(s: Status) -> Self {
        s.to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub gates: Gates,
    #[serde(default)]
    pub tests_patch_sha256: String,
    #[serde(default)]
    pub impl_patch_sha256: String,
    #[serde(default)]
    pub red_reason: String,
    /// Claude session id for the planner while iterating the spec. Kept
    /// across replans so iterations resume one session; cleared on spec
    /// approval. Empty = no open session.
    #[serde(default)]
    pub planner_session_id: String,
    /// Review findings already sent to a fix lane that then passed the green
    /// gate. Kept so the findings list shows what has been dealt with instead
    /// of asking again — and so a re-raised finding is visible as such.
    #[serde(default)]
    pub fixed_findings: Vec<crate::review::Finding>,
    /// sha256 of the spec the patches on disk were derived from. Compare it
    /// against spec.json now and you know whether a replan has moved the ground
    /// under them — an approval is only worth anything while it still describes
    /// what you read. Empty = nothing has been run yet.
    #[serde(default)]
    pub spec_sha_at_run: String,
    /// `git write-tree` of the index right after `stage` applied the patches.
    /// The commit that follows is only guvnor's to write while the index still
    /// hashes to this: past that point the staged change is yours, not the one
    /// a reviewer read. Empty = nothing staged.
    #[serde(default)]
    pub staged_tree: String,
}

/// Loose identity for a finding: file + note, case- and whitespace-insensitive.
/// Used only to mark a re-raised finding in the UI, so a near-miss costs
/// nothing — the reviewer's wording is not stable enough for anything stricter.
pub fn finding_key(f: &crate::review::Finding) -> String {
    format!("{}|{}", f.file.trim().to_lowercase(), f.note.trim().to_lowercase())
}

impl State {
    pub fn new(id: &str, title: &str) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: Status::Planned,
            gates: Gates::default(),
            tests_patch_sha256: String::new(),
            impl_patch_sha256: String::new(),
            red_reason: String::new(),
            planner_session_id: String::new(),
            fixed_findings: Vec::new(),
            spec_sha_at_run: String::new(),
            staged_tree: String::new(),
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
    repo.join(".guvnor/runs")
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
    fn finding_key_matches_across_reviewer_wording_noise() {
        use crate::review::{Finding, Severity};
        let f = |file: &str, note: &str| Finding {
            severity: Severity::High,
            file: file.into(),
            note: note.into(),
        };
        // case and surrounding whitespace are not signal; severity is not part
        // of identity (the reviewer re-grades the same issue differently)
        assert_eq!(finding_key(&f("src/a.js", " No Bounds Check ")), finding_key(&f("src/a.js", "no bounds check")));
        assert_ne!(finding_key(&f("src/a.js", "x")), finding_key(&f("src/b.js", "x")));
        assert_ne!(finding_key(&f("src/a.js", "x")), finding_key(&f("src/a.js", "y")));
    }

    #[test]
    fn status_roundtrips_as_flat_string() {
        for (s, json) in [
            (Status::Planned, "\"planned\""),
            (Status::Staged, "\"staged\""),
            (Status::Committed, "\"committed\""),
            (Status::Failed("vacuous_baseline".into()), "\"failed:vacuous_baseline\""),
            (Status::Failed("rejected_spec".into()), "\"failed:rejected_spec\""),
        ] {
            assert_eq!(serde_json::to_string(&s).unwrap(), json, "serialize {s:?}");
            assert_eq!(serde_json::from_str::<Status>(json).unwrap(), s, "deserialize {json}");
        }
        // Run dirs written before staging and committing were told apart: they
        // were staged, never committed, so they must keep reading as staged
        // rather than degrading into `Failed("merged")`.
        assert_eq!(serde_json::from_str::<Status>("\"merged\"").unwrap(), Status::Staged);
    }

    #[test]
    fn slug_basics() {
        assert_eq!(slugify("Add add3 to Lib!", 24), "add-add3-to-lib");
        assert_eq!(slugify("  weird---spaces  ", 8), "weird-s");
    }

    #[test]
    fn state_roundtrip_and_resolve() {
        let dir = std::env::temp_dir().join(format!("guvnor-st-{}", std::process::id()));
        let run = dir.join(".guvnor/runs/123-abc");
        std::fs::create_dir_all(&run).unwrap();
        let st = State::new("123-abc", "t");
        st.save(&run).unwrap();
        let loaded = State::load(&run).unwrap();
        assert_eq!(loaded.status, Status::Planned);
        let resolved = resolve_run_dir(&dir, "123").unwrap();
        assert!(resolved.ends_with("123-abc"));
        assert!(resolve_run_dir(&dir, "zzz").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
