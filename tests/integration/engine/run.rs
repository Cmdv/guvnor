use guvnor::config::{Claude, Commands, Config, Limits, Paths};
use guvnor::engine::run::{all_findings_are_tests, decorrelation_warning, seed_impl};
use guvnor::{git, review};

/// The amend path: re-running after a spec revision reuses the previous
/// implementation rather than paying for a cold pass. Every reason it can't
/// must fall back to cold, never fail — a bad seed is not a bad run.
#[test]
fn seeding_falls_back_to_cold_rather_than_failing() {
    let dir = std::env::temp_dir().join(format!("guvnor-seed-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let (run, wt) = (dir.join("run"), dir.join("wt"));
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    git::init_test_repo(&wt);
    git::ensure_baseline_commit(&wt).unwrap();

    // nothing to seed from
    assert!(!seed_impl(&wt, &run, ""));
    std::fs::write(run.join("impl.patch"), "").unwrap();
    assert!(!seed_impl(&wt, &run, ""), "an empty patch is not a seed");

    let patch = "diff --git a/src/a.js b/src/a.js\nnew file mode 100644\n--- /dev/null\n+++ b/src/a.js\n@@ -0,0 +1 @@\n+ok\n";
    std::fs::write(run.join("impl.patch"), patch).unwrap();
    // the new tests now own a file the old implementation created: seeding
    // would make the two patches non-composable and fail as if the
    // implementer had misbehaved, which is a lie about what happened
    assert!(!seed_impl(&wt, &run, patch), "overlap must start cold, not fail");
    // clean seed lands, and the file is really in the tree
    assert!(seed_impl(&wt, &run, "diff --git a/t/x b/t/x\n--- /dev/null\n+++ b/t/x\n"));
    assert_eq!(std::fs::read_to_string(wt.join("src/a.js")).unwrap(), "ok\n");
    // a patch that no longer applies (already applied) also goes cold
    assert!(!seed_impl(&wt, &run, ""));
    std::fs::remove_dir_all(&dir).ok();
}

fn cfg_with(worker: &str, reviewer: &str) -> Config {
    Config {
        commands: Commands { test: "true".into() },
        paths: Paths { tests: vec!["test/".into()], src: vec!["src/".into()] },
        claude: Claude {
            bin: "claude".into(),
            model_planner: "opus".into(),
            model_worker: worker.into(),
            model_reviewer: reviewer.into(),
        },
        limits: Limits::default(),
    }
}

#[test]
fn decorrelation_warning_fires_only_when_seats_match() {
    assert!(decorrelation_warning(&cfg_with("sonnet", "opus")).is_none());
    let w = decorrelation_warning(&cfg_with("sonnet", "sonnet")).unwrap();
    assert!(w.contains("sonnet"));
}

#[test]
fn a_fix_round_of_only_test_findings_is_a_dead_end() {
    let f = |file: &str| review::Finding {
        severity: review::Severity::Low,
        file: file.into(),
        note: "n".into(),
    };
    let tests = vec!["test/".into()];
    // every finding under the tests prefix, nothing else to do → dead end
    assert!(all_findings_are_tests(&[f("test/readme.test.js")], &tests));
    // one implementation finding is enough to give the lane real work
    assert!(!all_findings_are_tests(&[f("test/a.test.js"), f("src/x.js")], &tests));
    // no findings at all is not a test-only round (an instruction may carry it)
    assert!(!all_findings_are_tests(&[], &tests));
}
