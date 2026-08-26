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

/// Index of the `}` closing the object that starts at `start`, if it closes.
/// Braces inside string literals do not count, and `\"` inside one does not end
/// it. `depth` cannot underflow: the caller only passes a `{`, which takes it to
/// 1 before any `}` is reached. Both braces are ASCII, so every index the caller
/// slices at is a char boundary.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
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
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every JSON object in model output, in the order they appear. Models wrap
/// JSON in prose and fences despite instructions, and the reviewer's job is
/// reading diffs, which routinely contain JSON of their own, so the caller
/// takes the first that deserialises into the type it wants rather than
/// trusting the first that is merely valid.
///
/// Every `{` is a candidate, including one that never closes: an unmatched
/// brace in a prose snippet would otherwise swallow every object after it.
///
/// ponytail: O(n^2) on text that is mostly braces, which model replies are not.
/// If that ever bites, remember the end of the last yielded object and skip past
/// candidates inside it.
pub fn json_objects(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    (0..bytes.len())
        .filter(move |&i| bytes[i] == b'{')
        .filter_map(move |i| balanced_end(bytes, i).map(|end| &text[i..=end]))
        .filter(|s| serde_json::from_str::<serde_json::Value>(s).is_ok())
}
