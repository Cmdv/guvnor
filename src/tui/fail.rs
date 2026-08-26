//! The Failure tab: the failure a run is in, its evidence, and the way out.

use crate::state::{State, Status};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::*;

/// The failure a run is *in*, or `None`. Read from `status` — the log keeps
/// every failure forever, but a failure that has been fixed is history, not a
/// state, and a tab telling you what to do about it would be telling you to fix
/// something that already works. A rejected gate was never a failure at all.
pub fn active_failure(dir: &std::path::Path, st: &State) -> Option<(String, String)> {
    let Status::Failed(why) = &st.status else { return None };
    if why.starts_with("rejected_") {
        return None;
    }
    // Detail comes from the log; a `why` with nothing logged still earns a tab,
    // because the run is still broken either way.
    let detail = last_failure(dir)
        .filter(|(logged, _)| logged == why)
        .map(|(_, d)| d)
        .unwrap_or_default();
    Some((why.clone(), detail))
}

/// The Failure tab's body: the reason from `state.json`, the evidence from
/// `events.ndjson`, so it survives a restart. `None` once the run is no longer
/// failed — a fixed error is not something to keep offering advice about.
pub fn build_fail_tab(dir: &std::path::Path, st: &State) -> Option<Vec<Line<'static>>> {
    let (why, detail) = active_failure(dir, st)?;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" run failed: ", Style::new().fg(Color::Red).bold()),
            Span::styled(why.clone(), Style::new().fg(Color::Red).bold()),
        ]),
        Line::raw(""),
    ];
    lines.extend(detail.lines().map(|l| failure_line(strip_wt_paths(l).as_str())));
    // A fix round's failure is a conflict between a finding and a test, so the
    // finding is half the evidence. The Review tab's ticks are gone by now.
    let ticked = last_ticked(dir);
    if why == "fix_broke_tests" && !ticked.is_empty() {
        lines.push(Line::raw(""));
        lines.push(rule("the findings this round was told to fix", Color::Yellow));
        for t in &ticked {
            lines.push(Line::styled(format!("  · {t}"), Style::new().fg(Color::Cyan)));
        }
    }
    lines.push(Line::raw(""));
    lines.push(rule("what to do", Color::Yellow));
    for l in failure_advice(&why, &detail).lines() {
        lines.push(Line::styled(format!("  {l}"), Style::new().fg(Color::Yellow)));
    }
    Some(lines)
}

/// Advice per failure class, naming the key that fixes it — a machine reason
/// and a stack trace don't say which key to press.
pub fn failure_advice(why: &str, detail: &str) -> &'static str {
    // A lane that coded its refusal has already said which move fixes it —
    // that beats guessing from the failure class.
    if detail.contains("CANNOT/SPEC") {
        return "the lane refused because your instruction contradicts the spec, and only\nthe planner can change the spec. Go to the Review tab (←), keep the same\ninstruction, and pick `change the spec` instead of `fix the code`.";
    }
    if detail.contains("CANNOT/FENCED") {
        return "the lane refused because it needs an edit it is blocked from making —\ntests are written by an independent lane and it may never touch them. That\nis a spec change: Review tab (←), same instruction, `change the spec`.";
    }
    if detail.contains("CANNOT/UNCLEAR") {
        return "the lane could not tell what was being asked. Review tab (←), say it again\nmore concretely — name the file and the change.";
    }
    match why {
        "cancelled" => "you cancelled it — press r to run it again from the top",
        "vacuous_baseline" => {
            "your suite was already failing before guvnor touched anything.\nFix the tree first: guvnor needs a green baseline to prove red."
        }
        "vacuous_tests" => {
            "the new tests passed with no implementation, so they test nothing.\nSharpen the acceptance criteria on the Spec tab (i to iterate), then r."
        }
        "tests_lane_noop" | "impl_lane_noop" => {
            "the lane talked instead of editing — read its own words above.\nIf it refused, the spec asked for something it won't do: iterate on the\nSpec tab (i). Otherwise r runs it again."
        }
        "fix_lane_noop" => {
            "the fix lane edited nothing — read its own words above.\nIf it refused, your instruction and the spec disagree, or it needs a change\nit is fenced out of (tests, guvnor's own files). Go to the Review tab (←)\nand send a different instruction, or iterate the spec instead."
        }
        "impl_does_not_satisfy_tests" => {
            "the implementation could not pass the tests within the rework budget.\nRead the failing output above: if the tests are wrong the spec is wrong —\niterate it (Spec tab, i). If they're right, r to try again."
        }
        // What actually happened is that a finding contradicts a test — and the
        // fix lane never sees the tests, so only you can tell it that.
        "fix_broke_tests" => {
            "a finding you ticked cannot be true while a test passes. The fix lane never\nsees the tests, so it could not know — the failing one is named above, and\nyour implementation is still the one on disk.\n  · Review tab (←): put what you just read in the instruction box. That is the\n    only channel that reaches the lane, and it is usually enough.\n  · Or untick that finding: if a test depends on what the reviewer called\n    unnecessary, the reviewer was wrong and the code is right.\n  · If the test itself is wrong, that is `change the spec` — the lane may never\n    edit tests."
        }
        "impl_touched_test_files" | "tests_forbidden_paths" | "impl_forbidden_paths" => {
            "the two lanes fought over the same files. This is a spec problem:\nits Files list should name implementation files only. Iterate it (Spec tab, i)."
        }
        "review_unparseable" => "the reviewer didn't return valid JSON — press r to run it again",
        w if w.ends_with("_timeout") => {
            "the lane ran out of time. Raise limits.lane_timeout_secs in guvnor.toml\n(c on the runs list), or cut the spec down, then r."
        }
        _ => "read the evidence above, then iterate the spec (Spec tab, i) and r to re-run",
    }
}

