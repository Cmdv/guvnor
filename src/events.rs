use anyhow::Result;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Append-only NDJSON event log per run — the audit trail a human or a later
/// tool replays. Never rewritten, never truncated.
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(run_dir: &Path) -> Self {
        Self { path: run_dir.join("events.ndjson") }
    }

    pub fn append(&self, event: &str, data: serde_json::Value) -> Result<()> {
        let line = json!({
            "ts": now_iso(),
            "event": event,
            "data": data,
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

pub fn now_iso() -> String {
    // ponytail: epoch-seconds ISO without pulling in chrono; per-second
    // resolution is enough for an audit trail ordered by append.
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let days = secs / 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days-since-epoch → (y, m, d). Howard Hinnant's civil_from_days algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
