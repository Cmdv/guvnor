use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The reviewer's decision — a closed set; serde rejects anything else at
/// parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Decision {
    Approved,
    Warning,
    Blocked,
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // `pad`, not `write_str`: `write_str` bypasses the formatter and silently
        // ignores width, so `{:<7}` on one of these would produce
        // `lowsrc/numeric.js` in the findings list.
        f.pad(match self {
            Decision::Approved => "APPROVED",
            Decision::Warning => "WARNING",
            Decision::Blocked => "BLOCKED",
        })
    }
}

/// Finding severity — LLM-emitted, so an off-set value degrades to `Unknown`
/// rather than failing the whole review parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.pad(match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub verdict: Decision,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// Stored review: model verdict + OUR digest of the diff it judged.
/// Stage and commit refuse when the digest no longer matches the patches on
/// disk — a stale verdict can never ship a newer diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    #[serde(flatten)]
    pub verdict: Verdict,
    pub diff_sha256: String,
    pub model: String,
    pub ts: String,
}

/// The first JSON object in the reviewer's reply that is a verdict. Not the
/// first that is valid JSON: the reviewer is handed a diff to read, and a diff
/// that adds a JSON fixture would otherwise supply the object guvnor parses,
/// failing the run and throwing away three lanes' worth of tokens.
pub fn parse_verdict(result_text: &str) -> Result<Verdict> {
    crate::spec::json_objects(result_text)
        .find_map(|s| serde_json::from_str::<Verdict>(s).ok())
        .context("reviewer output contains no verdict object (need APPROVED|WARNING|BLOCKED)")
}
