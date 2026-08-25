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

    /// The reviewer is handed a diff to read, so other people's JSON turns up in
    /// its reply. Taking the first object that is merely valid hands back a
    /// fixture instead of the verdict, and the run dies as review_unparseable
    /// with three lanes already paid for.
    #[test]
    fn a_json_fixture_in_the_diff_is_not_mistaken_for_the_verdict() {
        let text = r#"The diff adds a fixture:
    +{"name": "widget", "qty": 2}
and a config block `{"debug": true}`. My assessment:
{"verdict": "WARNING", "summary": "s", "findings": []}"#;
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.verdict, Decision::Warning);
        assert_eq!(v.summary, "s");
    }

    /// An unmatched brace in a prose snippet must not swallow the answer after
    /// it. Scanning restarts one byte in, so the run below still finds it.
    #[test]
    fn an_unclosed_brace_before_the_verdict_is_stepped_over() {
        let text = "I looked at `fn main() {` and the guard `if x {`.\n\
                    {\"verdict\": \"APPROVED\", \"summary\": \"ok\"}";
        assert_eq!(parse_verdict(text).unwrap().verdict, Decision::Approved);
    }

    /// A finding with no `note` must not fail the whole review, same as `file`:
    /// both are fields a model fills in.
    #[test]
    fn a_finding_missing_its_note_still_parses() {
        let f: Finding = serde_json::from_str(r#"{"severity":"low","file":"a.rs"}"#).unwrap();
        assert_eq!(f.severity, Severity::Low);
        assert!(f.note.is_empty());
    }
}
