use crate::spec::extract_json_object;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: String,
    #[serde(default)]
    pub file: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub verdict: String,
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
    let v: Verdict = serde_json::from_str(json).context("verdict JSON shape invalid")?;
    match v.verdict.as_str() {
        "APPROVED" | "WARNING" | "BLOCKED" => Ok(v),
        other => bail!("verdict must be APPROVED|WARNING|BLOCKED, got '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_verdict_from_prose() {
        let text = r#"Here is my review:
{"verdict": "APPROVED", "summary": "ok", "findings": [{"severity": "low", "file": "src/a.rs", "note": "n"}]}"#;
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.verdict, "APPROVED");
        assert_eq!(v.findings.len(), 1);
    }

    #[test]
    fn rejects_bad_enum_and_missing_json() {
        assert!(parse_verdict(r#"{"verdict": "LGTM"}"#).is_err());
        assert!(parse_verdict("no json at all").is_err());
    }
}
