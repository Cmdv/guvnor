use guvnor::review::{parse_verdict, Decision, Finding, Severity};

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
