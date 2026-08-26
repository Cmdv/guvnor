use guvnor::casefile::{cost_rows, cost_summary, fmt_tok};

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
