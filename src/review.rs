use crate::spec::extract_json_object;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The reviewer's decision — a closed set the type now enforces (serde rejects
/// anything else at parse time, so no hand-rolled string validation).
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
        // ignores width, so `{:<7}` on one of these produced `lowsrc/numeric.js`
        // in the findings list.
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
/// Merge refuses when the digest no longer matches the patches on disk —
/// a stale verdict can never ship a newer diff (Foreman's admitted gate
/// hole, fixed by construction here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    #[serde(flatten)]
    pub verdict: Verdict,
    pub diff_sha256: String,
    pub model: String,
    pub ts: String,
}

pub fn parse_verdict(result_text: &str) -> Result<Verdict> {
    let json = extract_json_object(result_text)
        .context("reviewer output contains no JSON object")?;
    // serde enforces the APPROVED|WARNING|BLOCKED set via the Decision enum.
    serde_json::from_str(json).context("verdict JSON invalid (need APPROVED|WARNING|BLOCKED)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_verdict_from_prose() {
        let text = r#"Here is my review:
{"verdict": "APPROVED", "summary": "ok", "findings": [{"severity": "low", "file": "src/a.rs", "note": "n"}]}"#;
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.verdict, Decision::Approved);
        assert_eq!(v.findings.len(), 1);
    }

    #[test]
    fn severity_offset_degrades_to_unknown() {
        let f: Finding =
            serde_json::from_str(r#"{"severity":"critical","note":"n"}"#).unwrap();
        assert_eq!(f.severity, Severity::Unknown);
    }

    #[test]
    fn rejects_bad_enum_and_missing_json() {
        assert!(parse_verdict(r#"{"verdict": "LGTM"}"#).is_err());
        assert!(parse_verdict("no json at all").is_err());
    }
}
