use guvnor::state::{State, Status};
use guvnor::tui::fail::{active_failure, build_fail_tab, failure_advice};
use guvnor::tui::lines_text;

#[test]
fn a_failure_becomes_a_tab_with_a_way_forward() {
    let dir = std::env::temp_dir().join(format!("guvnor-failtab-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let mut st = State::new("20260101T000000-x", "t");
    let ev = |lines: &[&str]| std::fs::write(dir.join("events.ndjson"), lines.join("\n")).unwrap();

    // no failure recorded: no tab
    ev(&[r#"{"event":"baseline","data":{}}"#]);
    assert!(build_fail_tab(&dir, &st).is_none());

    ev(&[
        r#"{"event":"baseline","data":{}}"#,
        r#"{"event":"run_failed","data":{"why":"fix_lane_noop","detail":"no edits reached the tree\nwhat the lane said:\nI cannot remove the tests."}}"#,
    ]);
    st.status = Status::Failed("fix_lane_noop".into());
    st.save(&dir).unwrap();
    let text = lines_text(&build_fail_tab(&dir, &st).unwrap());
    assert!(text.contains("fix_lane_noop"));
    assert!(text.contains("I cannot remove the tests."), "the lane's own words: {text}");
    assert!(text.contains("what to do"), "a machine reason alone is a dead end");
    assert!(text.contains("Review tab"), "must name the key that fixes it");
    // every why code gets real advice, never the same sentence twice over
    for why in ["cancelled", "vacuous_tests", "fix_broke_tests", "review_timeout"] {
        assert_ne!(failure_advice(why, ""), failure_advice("something_new", ""), "{why}");
    }
    // a coded refusal overrides the failure class and names the button
    assert!(failure_advice("fix_lane_noop", "CANNOT/SPEC: needs LICENSE").contains("change the spec"));
    assert!(failure_advice("fix_lane_noop", "CANNOT/FENCED: tests").contains("change the spec"));

    // The advice must diagnose the finding/test conflict, never say
    // "narrower" — the fix is already narrow. What happened is that a ticked
    // finding contradicts a test, and the instruction box is the only
    // channel that reaches a lane which cannot see tests.
    let broke = failure_advice("fix_broke_tests", "");
    assert!(broke.contains("instruction box"), "{broke}");
    assert!(broke.contains("untick"), "{broke}");
    assert!(!broke.contains("narrower"), "'narrower' is a misdiagnosis: {broke}");

    // ...and the findings it was told to fix are half that evidence, so the
    // tab carries them: the Review tab's ticks are cleared by now
    ev(&[
        r#"{"event":"fix_started","data":{"findings":1,"note":"","ticked":[{"file":"src/numeric.js","note":"drop the + 0"}]}}"#,
        r#"{"event":"run_failed","data":{"why":"fix_broke_tests","detail":"✖ sqrt(-0) returns 0"}}"#,
    ]);
    st.status = Status::Failed("fix_broke_tests".into());
    st.save(&dir).unwrap();
    let text = lines_text(&build_fail_tab(&dir, &st).unwrap());
    assert!(text.contains("src/numeric.js — drop the + 0"), "{text}");
    assert!(text.contains("told to fix"), "{text}");

    // rejecting a gate is a decision, not a failure
    ev(&[r#"{"event":"run_failed","data":{"why":"rejected_work","detail":"nope"}}"#]);
    st.status = Status::Failed("rejected_work".into());
    assert!(build_fail_tab(&dir, &st).is_none());

    // fixed: the log still holds the failure forever, but the tab is gone —
    // advice about an error that no longer exists is worse than no tab
    ev(&[r#"{"event":"run_failed","data":{"why":"cancelled","detail":"d"}}"#]);
    st.status = Status::Reviewed;
    st.save(&dir).unwrap();
    assert!(build_fail_tab(&dir, &st).is_none(), "a fixed failure must stop showing");
    assert!(active_failure(&dir, &st).is_none());
    // ...and it comes back if the next attempt breaks again
    st.status = Status::Failed("cancelled".into());
    assert!(build_fail_tab(&dir, &st).is_some());
    // a status with nothing logged still earns a tab: the run is broken
    ev(&[r#"{"event":"baseline","data":{}}"#]);
    st.status = Status::Failed("impl_lane_timeout".into());
    assert!(build_fail_tab(&dir, &st).is_some(), "broken with no detail is still broken");
    std::fs::remove_dir_all(&dir).ok();
}
