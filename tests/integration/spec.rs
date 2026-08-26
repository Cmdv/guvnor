use guvnor::spec::{json_objects, Spec};

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
    let j = json_objects(t).next().unwrap();
    let v: serde_json::Value = serde_json::from_str(j).unwrap();
    assert_eq!(v["a"]["b"], 1);
}

#[test]
fn extract_skips_non_json_braces() {
    let t = "code { not json } then {\"ok\": true}";
    let j = json_objects(t).next().unwrap();
    assert_eq!(j, "{\"ok\": true}");
}

#[test]
fn extract_none_when_absent() {
    assert_eq!(json_objects("no braces here").count(), 0);
}
