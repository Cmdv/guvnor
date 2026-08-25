use crate::review::Review;
use crate::spec::Spec;
use crate::state::State;
use anyhow::Result;
use std::path::Path;

/// The human review surface: everything needed to judge a run, assembled from
/// on-disk artifacts. Claims come with evidence attached.
pub fn render(run_dir: &Path) -> Result<String> {
    let state = State::load(run_dir)?;
    let mut s = String::new();
    s.push_str(&format!("# Run {} — {}\nstatus: {}\n", state.id, state.title, state.status));

    if let Ok(spec) = Spec::load(&run_dir.join("spec.json")) {
        s.push_str("\n## Spec\n");
        s.push_str(&spec.render());
    }

    // The digest is taken from the bytes on the line above it. Carrying it in
    // state.json instead let the two drift: the patch is written to disk well
    // before the state is saved, so any error in between left this printing
    // yesterday's digest as fact.
    for (label, file) in [("Tests patch", "tests.patch"), ("Implementation patch", "impl.patch")] {
        if let Ok(patch) = std::fs::read_to_string(run_dir.join(file)) {
            let sha = crate::digest::sha256_hex(patch.as_bytes());
            let files = crate::worktree::patch_paths(&patch);
            let added = patch.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count();
            let removed = patch.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count();
            s.push_str(&format!(
                "\n## {label}\nsha256: {sha}\nfiles: {files:?}\n+{added} -{removed} lines ({file})\n"
            ));
        }
    }

    if !state.red_reason.is_empty() {
        s.push_str(&format!(
            "\n## Red evidence (tests failed on base, as required)\n```\n{}\n```\n",
            state.red_reason
        ));
    }
    if let Ok(green) = std::fs::read_to_string(run_dir.join("green.txt")) {
        // green.txt holds whatever the last green-gate run printed, pass or fail.
        // Only a run that got past the gate may call it evidence of passing;
        // before that it is the failure, and saying otherwise inverts the claim.
        let heading = if state.green_gate_passed() {
            "Green evidence (tests pass with implementation)"
        } else {
            "Green gate output (the tests did NOT pass)"
        };
        s.push_str(&format!("\n## {heading}\n```\n{green}\n```\n"));
    }

    if let Ok(raw) = std::fs::read_to_string(run_dir.join("review.json")) {
        if let Ok(review) = serde_json::from_str::<Review>(&raw) {
            s.push_str(&format!(
                "\n## Reviewer verdict: {}\n{}\ndiff_sha256: {} (model: {})\n",
                review.verdict.verdict, review.verdict.summary, review.diff_sha256, review.model
            ));
            for f in &review.verdict.findings {
                s.push_str(&format!("- [{}] {} — {}\n", f.severity, f.file, f.note));
            }
        }
    }

    if let Some(cost) = cost_summary(run_dir) {
        s.push_str(&format!("\n## Tokens / cost (all attempts)\n{cost}\n"));
    }

    s.push_str(&format!(
        "\n## Gates\nspec:  {}\ntests: {}  (do these test the spec, not trivia?)\nwork:  {}  (is the implementation right?)\n",
        mark(&state.gates.spec),
        mark(&state.gates.tests),
        mark(&state.gates.work)
    ));
    Ok(s)
}

/// Token counts as `159.9k`. Raw below 1000 — `0.4k` for 350 would be worse
/// than the number it replaces.
pub fn fmt_tok(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// Total spend for a run: every cost_usd in events.ndjson, whatever the lane.
pub fn total_cost(run_dir: &Path) -> f64 {
    let Ok(raw) = std::fs::read_to_string(run_dir.join("events.ndjson")) else { return 0.0 };
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| v["data"]["cost_usd"].as_f64().unwrap_or(0.0))
        .sum()
}

/// Per-lane token/cost ledger summed from events.ndjson. Repeated lanes are
/// numbered in the order they happened, so a spec that took four planner passes
/// and an implementation that took three reviews read as such:
///
/// ```text
/// spec draft          159.9k → 1.2k    tok   $0.4000
/// spec revision 1       3.4k → 402     tok   $0.0100
/// review 1              8.1k → 300     tok   $0.0200
/// fix                   6.0k → 512     tok   $0.0150
/// ```
pub fn cost_summary(run_dir: &Path) -> Option<String> {
    let rows = cost_rows(run_dir);
    if rows.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| {
            format!("{:<18} {:>8} → {:<7} tok   ${:.4}", r.name, fmt_tok(r.tin), fmt_tok(r.tout), r.cost)
        })
        .collect();
    let (tin, tout, cost) = cost_total(&rows);
    lines.push(format!(
        "{:<18} {:>8} → {:<7} tok   ${cost:.4}",
        "total",
        fmt_tok(tin),
        fmt_tok(tout)
    ));
    Some(lines.join("\n"))
}

/// One lane's spend. Kept structured so the TUI can put it in a real table
/// with a header that stays put, instead of re-parsing formatted text.
pub struct CostRow {
    pub name: String,
    pub tin: u64,
    pub tout: u64,
    pub cost: f64,
}

/// Column sums — the one number that must stay on screen however far the
/// ledger scrolls.
pub fn cost_total(rows: &[CostRow]) -> (u64, u64, f64) {
    rows.iter().fold((0, 0, 0.0), |(i, o, c), r| (i + r.tin, o + r.tout, c + r.cost))
}

/// The ledger, one row per lane pass, in the order they happened.
pub fn cost_rows(run_dir: &Path) -> Vec<CostRow> {
    let Ok(raw) = std::fs::read_to_string(run_dir.join("events.ndjson")) else { return Vec::new() };
    let mut rows = Vec::new();
    // how many times each lane has been seen, so pass N can be named
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    // Fix rounds you asked for, counted separately from the retries the engine
    // does inside one of them — "why did that button press cost $0.20" is a
    // question the ledger should answer.
    let mut fix_n = 0u32;
    for l in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
        let event = v["event"].as_str().unwrap_or("").to_string();
        if !matches!(
            event.as_str(),
            "lane_planner" | "lane_tests" | "lane_impl" | "lane_impl_rework" | "lane_impl_fix" | "reviewed"
        ) {
            continue;
        }
        let d = &v["data"];
        let (i, o, c) = (
            d["tokens_in"].as_u64().unwrap_or(0),
            d["tokens_out"].as_u64().unwrap_or(0),
            d["cost_usd"].as_f64().unwrap_or(0.0),
        );
        if i == 0 && o == 0 {
            continue; // no usage data: nothing to put in the ledger
        }
        let n = *seen.entry(event.clone()).and_modify(|n| *n += 1).or_insert(1);
        // a missing `round` reads as 0
        let round = d["round"].as_u64().unwrap_or(0);
        if event == "lane_impl_fix" && round == 0 {
            fix_n += 1;
        }
        // the first planner pass is the draft; every later one is a revision
        let name = match (event.as_str(), n) {
            ("lane_planner", 1) => "spec draft".to_string(),
            ("lane_planner", n) => format!("spec revision {}", n - 1),
            ("lane_tests", 1) => "test-writer".to_string(),
            ("lane_tests", n) => format!("test-writer {n}"),
            ("lane_impl", 1) => "implementer".to_string(),
            ("lane_impl", n) => format!("implementer {n}"),
            ("lane_impl_rework", n) => format!("rework {n}"),
            ("lane_impl_fix", _) if round > 0 => format!("fix {fix_n} retry {round}"),
            ("lane_impl_fix", _) if fix_n == 1 => "fix".to_string(),
            ("lane_impl_fix", _) => format!("fix {fix_n}"),
            ("reviewed", 1) => "review".to_string(),
            ("reviewed", n) => format!("review {n}"),
            // Named, not caught: a `_` arm here would file the next lane anyone
            // adds under "review" in the ledger a human reads to check costs.
            _ => continue,
        };
        rows.push(CostRow { name, tin: i, tout: o, cost: c });
    }
    rows
}

fn mark(a: &crate::state::Approval) -> String {
    if a.approved {
        format!("approved {} {}", a.ts, a.note)
    } else {
        "pending".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_summary_sums_lane_events_and_skips_rows_without_usage() {
        let dir = std::env::temp_dir().join(format!("guvnor-cost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("events.ndjson"),
            concat!(
                r#"{"ts":"t","event":"lane_planner","data":{"tokens_in":100,"tokens_out":10,"cost_usd":0.01}}"#, "\n",
                r#"{"ts":"t","event":"lane_tests","data":{"changed":true}}"#, "\n", // event without usage data
                r#"{"ts":"t","event":"lane_impl","data":{"tokens_in":200,"tokens_out":20,"cost_usd":0.02}}"#, "\n",
                r#"{"ts":"t","event":"reviewed","data":{"verdict":"APPROVED","tokens_in":50,"tokens_out":5,"cost_usd":0.005}}"#, "\n",
            ),
        )
        .unwrap();
        let s = cost_summary(&dir).unwrap();
        assert!(s.contains("spec draft"));
        assert!(!s.contains("test-writer"), "an event without usage data must be skipped");
        assert!(s.contains("350"), "under 1k stays raw: {s}");
        assert!(s.contains("$0.0350"), "total cost summed: {s}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tokens_render_as_k_above_a_thousand() {
        assert_eq!(fmt_tok(159_944), "159.9k");
        assert_eq!(fmt_tok(1_000), "1.0k");
        assert_eq!(fmt_tok(1_050), "1.1k"); // rounds, not truncates
        // below 1k the abbreviation loses more than it saves
        assert_eq!(fmt_tok(999), "999");
        assert_eq!(fmt_tok(0), "0");
    }

    #[test]
    fn cost_summary_numbers_repeated_lanes() {
        let dir = std::env::temp_dir().join(format!("guvnor-costn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ev = |e: &str| {
            format!(
                r#"{{"ts":"t","event":"{e}","data":{{"tokens_in":10,"tokens_out":1,"cost_usd":0.001}}}}"#
            )
        };
        std::fs::write(
            dir.join("events.ndjson"),
            [
                ev("lane_planner"),   // draft
                ev("lane_planner"),   // spec iteration 1
                ev("lane_planner"),   // spec iteration 2
                ev("lane_impl"),
                ev("reviewed"),       // review 1
                ev("lane_impl_fix"),  // took the review's advice
                ev("reviewed"),       // review 2
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let s = cost_summary(&dir).unwrap();
        // spec iterations are individually attributable
        assert!(s.contains("spec draft"), "{s}");
        assert!(s.contains("spec revision 1"), "{s}");
        assert!(s.contains("spec revision 2"), "{s}");
        // so are review rounds and the fix between them
        assert!(s.contains("review\n") || s.contains("review "), "{s}");
        assert!(s.contains("review 2"), "{s}");
        assert!(s.contains("fix"), "{s}");
        assert!(s.contains("$0.0070"), "total across 7 lanes: {s}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fix round can retry itself when it breaks the suite, so "why did that
    /// one button press cost twice as much" has to be answerable: the retries
    /// are named as retries, not as fix rounds you asked for.
    #[test]
    fn fix_retries_are_told_apart_from_fix_rounds() {
        let dir = std::env::temp_dir().join(format!("guvnor-costfix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ev = |e: &str, round: u64| {
            format!(
                r#"{{"ts":"t","event":"{e}","data":{{"round":{round},"tokens_in":10,"tokens_out":1,"cost_usd":0.001}}}}"#
            )
        };
        std::fs::write(
            dir.join("events.ndjson"),
            [
                ev("lane_impl_fix", 0), // you pressed it
                ev("lane_impl_fix", 1), // it broke the suite; the engine retried
                ev("lane_impl_fix", 0), // you pressed it again
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let names: Vec<String> = cost_rows(&dir).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["fix", "fix 1 retry 1", "fix 2"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cost_summary_none_without_metrics() {
        let dir = std::env::temp_dir().join(format!("guvnor-cost0-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.ndjson"), "{\"ts\":\"t\",\"event\":\"baseline\",\"data\":{}}\n").unwrap();
        assert!(cost_summary(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
