use guvnor::lane::{
    absorb_result_event, commit_msg_prompt, fix_prompt, implementer_prompt, planner_prompt,
    reviewer_prompt, rework_prompt, run, testwriter_prompt, write_settings, LaneSpec, ReaderOut,
    Session, FENCE,
};
use std::sync::mpsc;
use std::time::Duration;

/// A path with a comma broke the old `GUVNOR_DENY=<csv>` command line; a
/// NUL-joined file has no such delimiter collision, and the deny list no
/// longer appears on the command line at all.
#[test]
fn write_settings_puts_deny_in_a_nul_joined_file_not_on_the_command_line() {
    let dir = std::env::temp_dir().join("guvnor-lane-deny-test");
    std::fs::create_dir_all(&dir).unwrap();
    write_settings(&dir, &["test/a, comma.js".into(), "test/b.js".into()]).unwrap();
    let settings = std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap();
    assert!(!settings.contains("GUVNOR_DENY"), "deny must not be on the command line");
    let deny = std::fs::read_to_string(dir.join(".claude/deny")).unwrap();
    assert_eq!(deny, "test/a, comma.js\0test/b.js");
}

#[test]
fn write_settings_skips_the_deny_file_when_nothing_is_denied() {
    let dir = std::env::temp_dir().join("guvnor-lane-nodeny-test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::remove_file(dir.join(".claude/deny")).ok();
    write_settings(&dir, &[]).unwrap();
    assert!(!dir.join(".claude/deny").exists());
}

#[test]
fn tail_last_lines() {
}

#[test]
fn absorbs_usage_from_real_result_shape() {
    // Field layout taken from a real claude CLI 2.1.234 result event.
    let v: serde_json::Value = serde_json::from_str(
        r#"{"type":"result","total_cost_usd":0.0382787,"usage":{"input_tokens":33,"cache_creation_input_tokens":9357,"cache_read_input_tokens":103897,"output_tokens":1548},"result":"IMPL_READY: src/mathx.js"}"#,
    )
    .unwrap();
    let mut out = ReaderOut::default();
    absorb_result_event(&v, &mut out);
    assert_eq!(out.result_text, "IMPL_READY: src/mathx.js");
    assert_eq!(out.tokens_in, 33 + 9357 + 103897);
    assert_eq!(out.tokens_out, 1548);
    assert!((out.cost_usd - 0.0382787).abs() < 1e-9);
}

#[test]
fn line_sink_forwards_stdout() {
    let dir = std::env::temp_dir().join("guvnor-lane-sink-test");
    std::fs::create_dir_all(&dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let res = run(LaneSpec {
        cwd: &dir,
        claude_bin: "echo", // prints its args (our flags + prompt) to stdout
        model: "none",
        prompt: "SINKPROBE".into(),
        allowed_tools: "",
        timeout: Duration::from_secs(10),
        transcript: dir.join("t.ndjson"),
        line_sink: Some(Box::new(move |l| {
            let _ = tx.send(l);
        })),
        session: Session::Ephemeral,
    })
    .unwrap();
    assert_eq!(res.exit_code, Some(0));
    let lines: Vec<String> = rx.try_iter().collect();
    assert!(lines.iter().any(|l| l.contains("SINKPROBE")));
}

#[test]
fn session_flags_map_to_cli() {
    // `echo` prints our argv back, so the session flags are observable.
    let cases = [
        (Session::Ephemeral, "--no-session-persistence", ""),
        (Session::Create("sid-1".into()), "--session-id", "sid-1"),
        (Session::Resume("sid-2".into()), "--resume", "sid-2"),
    ];
    for (session, flag, id) in cases {
        let dir = std::env::temp_dir().join("guvnor-lane-sess-test");
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        run(LaneSpec {
            cwd: &dir,
            claude_bin: "echo",
            model: "none",
            prompt: "P".into(),
            allowed_tools: "",
            timeout: Duration::from_secs(10),
            transcript: dir.join("t.ndjson"),
            line_sink: Some(Box::new(move |l| {
                let _ = tx.send(l);
            })),
            session,
        })
        .unwrap();
        let joined = rx.try_iter().collect::<Vec<_>>().join(" ");
        assert!(joined.contains(flag), "missing {flag} in: {joined}");
        assert!(joined.contains(id), "missing id {id} in: {joined}");
    }
}

#[test]
fn prompts_lead_with_constraints() {
    let t = testwriter_prompt("SPEC", &["test/".into()], "node --test");
    assert!(t.find("HARD CONSTRAINTS").unwrap() < t.find("SPEC").unwrap());
    let i = implementer_prompt("SPEC", &["src/".into()], "node --test");
    // every writer lane states the fence the guards enforce, shell included
    let rw = rework_prompt("SPEC", &["src/".into()], "node --test", "fail", 1, 1);
    for p in [&t, &i, &rw] {
        assert!(p.contains(FENCE), "prompt is missing the fence");
    }
    assert!(i.contains("Do NOT create or modify test files"));
    // the spec's Files list names test files; the prompt must override it
    assert!(i.contains("ignore those entries"));
    let r = reviewer_prompt("SPEC", "DIFF", "node --test", "7 pass 0 fail");
    assert!(r.contains("UNTRUSTED"));
    // a test file absent from the Files list is not a scope violation
    assert!(r.contains("list membership"));
    // the planner must not emit file-manifest acceptance criteria
    assert!(planner_prompt("t", "ctx", "node --test").contains("file manifest"));
}

/// The reviewer has no Bash by design; without the green gate's evidence in
/// the prompt it files its own denied Bash as findings. It needs the gate's
/// evidence and an explicit ban, not a shell.
#[test]
fn reviewer_prompt_carries_the_green_evidence_instead_of_a_shell() {
    let r = reviewer_prompt("SPEC", "DIFF", "node --test", "# pass 7\n# fail 0");
    assert!(r.contains("# pass 7"), "the harness output is the evidence");
    assert!(r.contains("node --test"), "name the command that was run");
    assert!(r.contains("NO shell"));
    assert!(r.contains("NEVER report being unable to run tests"));
}

/// The message ends up in git history, where nothing about guvnor's process
/// exists. A prompt that ships the spec invites "satisfies criterion 7",
/// which is noise to everyone who ever reads that commit.
#[test]
fn commit_msg_prompt_keeps_the_process_out_of_git_history() {
    let p = commit_msg_prompt("add rolling stats", "diff --git a/src/a.js b/src/a.js");
    assert!(p.contains("add rolling stats"), "intent is context");
    assert!(p.contains("MAXIMUM 80 characters"));
    // the ban is explicit and names what leaks
    for banned in ["acceptance criteria", "criterion numbers", "gates", "reviews"] {
        assert!(p.contains(banned), "the rule must name {banned}");
    }
    // and the spec itself never gets shipped: no headings a spec render has
    for leak in ["## Spec", "Acceptance criteria", "Interfaces", "Verification"] {
        assert!(!p.contains(leak), "spec content reached the prompt: {leak}");
    }
}

#[test]
fn fix_prompt_carries_only_the_selected_findings() {
    use guvnor::review::{Finding, Severity};
    let picked = [
        Finding { severity: Severity::High, file: "src/a.js".into(), note: "off by one".into() },
        Finding { severity: Severity::Low, file: String::new(), note: "naming".into() },
    ];
    let p = fix_prompt("SPEC", &["src/".into()], "node --test", &picked, &[], "", None);
    assert!(p.find("HARD CONSTRAINTS").unwrap() < p.find("SPEC").unwrap());
    assert!(p.contains("[high] in src/a.js: off by one"));
    // a finding with no file must not render a dangling " in "
    assert!(p.contains("[low]: naming"));
    // reviewer prose reaches a writing lane: it must be fenced as data
    assert!(p.contains("UNTRUSTED DATA"));
    // the fix lane must not weaken the tests it cannot see
    assert!(p.contains("Do not create or modify tests"));
    assert!(p.contains("tests must still pass"));
    // no operator note: no dangling section, no dangling clause
    assert!(!p.contains("Operator instruction"));
    assert!(!p.contains("operator instruction below"));

    // nothing fixed yet: no history section to confuse the lane
    assert!(!p.contains("earlier fix rounds"));
    // a lane that can't do what was asked must say so, not fake a diff
    assert!(p.contains("CANNOT/SPEC:") && p.contains("CANNOT/FENCED:"));
    // first attempt: nothing has broken yet, so no regression section
    assert!(!p.contains("BROKE A TEST"));

    let w = fix_prompt("SPEC", &["src/".into()], "node --test", &picked, &[], "  use a Map  ", None);
    assert!(w.contains("Operator instruction"));
    assert!(w.contains("use a Map"));
    // the human's words are trusted; the reviewer's are not
    assert!(w.contains("trusted"));
    assert!(w.contains("UNTRUSTED DATA"));

    // round two: what round one fixed must be named, or it gets undone
    let done = [Finding { severity: Severity::High, file: "src/b.js".into(), note: "guard".into() }];
    let h = fix_prompt("SPEC", &["src/".into()], "node --test", &picked, &done, "", None);
    assert!(h.contains("Already addressed in earlier fix rounds"));
    assert!(h.contains("[high] in src/b.js: guard"));
}

/// Without the regression context the lane cannot see what broke and makes
/// the identical edit again, and the human has to re-type the failure by
/// hand.
#[test]
fn a_fix_that_broke_a_test_gets_told_which_one() {
    use guvnor::review::{Finding, Severity};
    let picked =
        [Finding { severity: Severity::Low, file: "src/a.js".into(), note: "drop the + 0".into() }];
    let tail = "✖ sqrt(-0) returns 0\nAssertionError: Expected values to be strictly equal";
    let p = fix_prompt("SPEC", &["src/".into()], "node --test", &picked, &[], "", Some(tail));
    assert!(p.contains("BROKE A TEST"));
    assert!(p.contains("sqrt(-0) returns 0"), "the failing test itself must be in there");
    // failing output is machine-written: fenced like every other model text
    assert!(p.contains("UNTRUSTED DATA"));
    // and the way out when the finding and the test cannot both be true —
    // forcing it is what took the suite down in the first place
    assert!(p.contains("CANNOT/FENCED:"));
    assert!(p.contains("the test decides"), "{p}");
    // the findings are still the job; the regression is context, not a swap
    assert!(p.contains("[low] in src/a.js: drop the + 0"));
}
