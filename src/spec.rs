use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The five-part spec: Objective, Files, Interfaces,
/// Constraints, Verification — plus acceptance criteria the reviewer scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub title: String,
    pub objective: String,
    /// Files expected to change (informative, not enforced — path bans are).
    pub files: Vec<String>,
    /// Signatures / API shapes the implementation must expose.
    pub interfaces: Vec<String>,
    pub constraints: Vec<String>,
    /// Exact command that proves the work (defaults to config test cmd).
    pub verification: String,
    pub acceptance_criteria: Vec<String>,
}

impl Spec {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let spec: Spec = serde_json::from_str(&raw).context("spec.json parse error")?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() || self.objective.trim().is_empty() {
            bail!("spec needs a non-empty title and objective");
        }
        if self.acceptance_criteria.is_empty() {
            bail!("spec needs at least one acceptance criterion");
        }
        if self.verification.trim().is_empty() {
            bail!("spec needs a verification command");
        }
        Ok(())
    }

    /// Render for lane prompts: stable, human-readable, no JSON noise.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("# {}\n\n## Objective\n{}\n", self.title, self.objective));
        s.push_str("\n## Files (expected to change)\n");
        for f in &self.files {
            s.push_str(&format!("- {f}\n"));
        }
        s.push_str("\n## Interfaces\n");
        for i in &self.interfaces {
            s.push_str(&format!("- {i}\n"));
        }
        s.push_str("\n## Constraints\n");
        for c in &self.constraints {
            s.push_str(&format!("- {c}\n"));
        }
        s.push_str(&format!("\n## Verification\n{}\n", self.verification));
        s.push_str("\n## Acceptance criteria\n");
        for (n, a) in self.acceptance_criteria.iter().enumerate() {
            s.push_str(&format!("{}. {a}\n", n + 1));
        }
        s
    }
}

/// Extract the first JSON object from model output text (models wrap JSON in
/// prose/fences despite instructions). Scans for balanced braces from each
/// '{' and returns the first slice that parses.
pub fn extract_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if start.is_none() {
            if b == b'{' {
                start = Some(i);
                depth = 1;
                in_str = false;
                esc = false;
            }
            continue;
        }
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    let s = &text[start.unwrap()..=i];
                    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
                        return Some(s);
                    }
                    start = None; // keep scanning past a non-JSON brace blob
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            title: "t".into(),
            objective: "o".into(),
            files: vec![],
            interfaces: vec![],
            constraints: vec![],
            verification: "node --test".into(),
            acceptance_criteria: vec!["works".into()],
        }
    }

    #[test]
    fn validate_rejects_empty_criteria() {
        let mut s = spec();
        s.acceptance_criteria.clear();
        assert!(s.validate().is_err());
    }

    #[test]
    fn render_contains_all_parts() {
        let r = spec().render();
        for h in ["Objective", "Interfaces", "Constraints", "Verification", "Acceptance"] {
            assert!(r.contains(h), "missing {h}");
        }
    }

    #[test]
    fn extract_json_from_prose_and_fences() {
        let t = "Sure! Here you go:\n```json\n{\"a\": {\"b\": 1}, \"s\": \"x } y\"}\n```\nDone.";
        let j = extract_json_object(t).unwrap();
        let v: serde_json::Value = serde_json::from_str(j).unwrap();
        assert_eq!(v["a"]["b"], 1);
    }

    #[test]
    fn extract_skips_non_json_braces() {
        let t = "code { not json } then {\"ok\": true}";
        let j = extract_json_object(t).unwrap();
        assert_eq!(j, "{\"ok\": true}");
    }

    #[test]
    fn extract_none_when_absent() {
        assert!(extract_json_object("no braces here").is_none());
    }
}
