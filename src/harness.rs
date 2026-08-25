use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct TestOutcome {
    pub green: bool,
    pub exit_code: Option<i32>,
    /// Last lines of combined output — the evidence a human sees in the case file.
    pub tail: String,
    /// The command ran past its deadline and was killed. Not green, and worth
    /// saying so separately: a suite that hangs looks nothing like one that fails.
    pub timed_out: bool,
}

/// How many trailing output lines become the evidence.
const TAIL: usize = 40;

/// Run the configured test command via `sh -c` in a worktree, bounded by
/// `timeout`. The command comes from config and the files it executes are
/// written by lanes, so neither the runtime nor the volume is trustworthy: the
/// deadline kills the whole process group, and only the tail is retained rather
/// than buffering everything a chatty suite prints.
pub fn run_tests(dir: &Path, test_cmd: &str, timeout: Duration) -> Result<TestOutcome> {
    let mut child = Command::new("sh")
        .args(["-c", test_cmd])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("failed to spawn: sh -c '{test_cmd}'"))?;
    let pgid = child.id() as i32;

    // One capped buffer per stream, each drained on its own thread so neither
    // pipe can fill and stall the child.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut keep: VecDeque<String> = VecDeque::with_capacity(TAIL);
            if let Some(p) = pipe {
                crate::lane::for_each_line(p, |line| {
                    if keep.len() == TAIL {
                        keep.pop_front();
                    }
                    keep.push_back(line);
                });
            }
            keep
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            crate::lane::kill_group(pgid, &mut child);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let mut lines: Vec<String> = out.join().unwrap_or_default().into();
    lines.extend(err.join().unwrap_or_default());
    let start = lines.len().saturating_sub(TAIL);
    let mut tail = lines[start..].join("\n");
    if timed_out {
        tail.push_str(&format!("\n\nguvnor: killed after {}s", timeout.as_secs()));
    }
    Ok(TestOutcome {
        green: !timed_out && status.is_some_and(|s| s.success()),
        exit_code: status.and_then(|s| s.code()),
        tail,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_command_is_green_and_keeps_its_output() {
        let o = run_tests(Path::new("."), "echo hello; exit 0", Duration::from_secs(30)).unwrap();
        assert!(o.green && !o.timed_out);
        assert_eq!(o.exit_code, Some(0));
        assert!(o.tail.contains("hello"), "{:?}", o.tail);
    }

    #[test]
    fn a_failing_command_is_not_green_and_keeps_stderr() {
        let o = run_tests(Path::new("."), "echo boom >&2; exit 3", Duration::from_secs(30)).unwrap();
        assert!(!o.green && !o.timed_out);
        assert_eq!(o.exit_code, Some(3));
        assert!(o.tail.contains("boom"), "{:?}", o.tail);
    }

    /// A hung suite must not hang guvnor: without a deadline this blocks at the
    /// baseline, red or green gate forever.
    #[test]
    fn a_hanging_command_is_killed_not_waited_on() {
        let started = Instant::now();
        let o = run_tests(Path::new("."), "sleep 30", Duration::from_millis(300)).unwrap();
        assert!(o.timed_out && !o.green, "{:?}", o.tail);
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert!(o.tail.contains("killed after"), "{:?}", o.tail);
    }

    /// The child's children die with it, or a killed suite leaves its test server
    /// holding the port.
    #[test]
    fn the_whole_process_group_goes_not_just_the_shell() {
        let marker = std::env::temp_dir().join(format!("guvnor-harness-{}", std::process::id()));
        std::fs::remove_file(&marker).ok();
        let cmd = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let o = run_tests(Path::new("."), &cmd, Duration::from_millis(300)).unwrap();
        assert!(o.timed_out);
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "a grandchild outlived the kill");
    }

    /// Unbounded output is a memory leak in the middle of the pipeline.
    #[test]
    fn output_is_capped_to_the_tail() {
        let o = run_tests(Path::new("."), "seq 1 5000", Duration::from_secs(60)).unwrap();
        assert_eq!(o.tail.lines().count(), TAIL);
        assert!(o.tail.contains("5000") && !o.tail.contains("\n1\n"), "keeps the END: {:?}", o.tail);
    }
}
