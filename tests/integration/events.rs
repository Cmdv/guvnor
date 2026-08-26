use guvnor::events::{civil_from_days, EventLog};
use serde_json::json;

#[test]
fn civil_epoch_and_known_date() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(19_723), (2024, 1, 1));
}

#[test]
fn append_writes_ndjson_lines() {
    let dir = std::env::temp_dir().join(format!("guvnor-ev-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = EventLog::new(&dir);
    log.append("a", json!({"x": 1})).unwrap();
    log.append("b", json!({})).unwrap();
    let raw = std::fs::read_to_string(dir.join("events.ndjson")).unwrap();
    let lines: Vec<_> = raw.lines().collect();
    assert_eq!(lines.len(), 2);
    for l in lines {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        assert!(v["ts"].as_str().unwrap().ends_with('Z'));
    }
    std::fs::remove_dir_all(&dir).ok();
}
