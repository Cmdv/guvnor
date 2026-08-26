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
        // One write, not writeln!'s two: under O_APPEND a single write cannot
        // interleave with another process's, and this log is the audit trail.
        f.write_all(format!("{line}\n").as_bytes())?;
        Ok(())
    }
}

pub fn now_iso() -> String {
    // Epoch-seconds ISO, no chrono dependency. Two events in the same second
    // are still ordered correctly: append order to events.ndjson IS the sort
    // key, so sub-second precision would add nothing.
    // A clock before 1970 is not worth a panic in the middle of a run.
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
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
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
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
