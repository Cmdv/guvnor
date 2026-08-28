// from src/tui/mod.rs — items reached via guvnor::tui::* (this file has no "mod" path segment of its own)

use guvnor::engine::Progress;
use guvnor::state::State;
use guvnor::tui::{App, Job, JobKind, Outcome, Screen, REVIEW_TAB};

/// `engine::fix`'s precondition checks (e.g. "every ticked finding is a test
/// file") return `Err` before any lane runs — nothing on disk changes. That
/// used to bounce the whole run screen back to the run list; it should land
/// you back on the run you were looking at instead, same as a lane that
/// actually ran and failed already does for the Failure tab.
#[test]
fn a_precondition_error_returns_to_the_run_not_home() {
    let repo = std::env::temp_dir().join(format!("guvnor-error-nav-{}", std::process::id()));
    std::fs::remove_dir_all(&repo).ok();
    let id = "20260101T000000-x";
    let run_dir = repo.join(".guvnor/runs").join(id);
    std::fs::create_dir_all(&run_dir).unwrap();
    State::new(id, "t").save(&run_dir).unwrap();
    // Just enough for the Review tab to be live: build_case only needs
    // review.json to parse; tests.patch/impl.patch fall back to "no patch
    // recorded" when absent.
    std::fs::write(
        run_dir.join("review.json"),
        br#"{"verdict":"APPROVED","summary":"ok","findings":[],"diff_sha256":"deadbeef","model":"opus","ts":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();

    let mut app = App::for_test();
    app.repo = repo.clone();
    app.screen = Screen::Progress;
    let (_tx, rx) = std::sync::mpsc::channel::<Progress>();
    app.job = Some(Job {
        kind: JobKind::Fix,
        run_id: Some(id.into()),
        rx,
        handle: None,
        started: std::time::Instant::now(),
        log: Vec::new(),
        lane: String::new(),
        tail: Default::default(),
        denials: 0,
        tools: 0,
        outcome: Some(Outcome::Error("every ticked finding is about a test file".into())),
    });

    app.maybe_finish();

    match &app.screen {
        Screen::Case(v) => {
            assert_eq!(v.id, id, "back on the same run");
            assert_eq!(v.tab, REVIEW_TAB, "review is live, so it lands there, not tab 0");
        }
        _ => panic!("a rejected precondition must not bounce the run screen home"),
    }

    std::fs::remove_dir_all(&repo).ok();
}
