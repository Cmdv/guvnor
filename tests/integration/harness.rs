use guvnor::harness::{run_tests, TAIL};
use std::path::Path;
use std::time::{Duration, Instant};

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
