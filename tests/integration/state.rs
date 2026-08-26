use guvnor::state::{finding_key, resolve_run_dir, slugify, State, Status};

#[test]
fn finding_key_matches_across_reviewer_wording_noise() {
    use guvnor::review::{Finding, Severity};
    let f = |file: &str, note: &str| Finding {
        severity: Severity::High,
        file: file.into(),
        note: note.into(),
    };
    // case and surrounding whitespace are not signal; severity is not part
    // of identity (the reviewer re-grades the same issue differently)
    assert_eq!(finding_key(&f("src/a.js", " No Bounds Check ")), finding_key(&f("src/a.js", "no bounds check")));
    assert_ne!(finding_key(&f("src/a.js", "x")), finding_key(&f("src/b.js", "x")));
    assert_ne!(finding_key(&f("src/a.js", "x")), finding_key(&f("src/a.js", "y")));
}

#[test]
fn status_roundtrips_as_flat_string() {
    for (s, json) in [
        (Status::Planned, "\"planned\""),
        (Status::Staged, "\"staged\""),
        (Status::Committed, "\"committed\""),
        (Status::Failed("vacuous_baseline".into()), "\"failed:vacuous_baseline\""),
        (Status::Failed("rejected_spec".into()), "\"failed:rejected_spec\""),
    ] {
        assert_eq!(serde_json::to_string(&s).unwrap(), json, "serialize {s:?}");
        assert_eq!(serde_json::from_str::<Status>(json).unwrap(), s, "deserialize {json}");
    }
}

#[test]
fn slug_basics() {
    assert_eq!(slugify("Add add3 to Lib!", 24), "add-add3-to-lib");
    assert_eq!(slugify("  weird---spaces  ", 8), "weird-s");
}

#[test]
fn state_roundtrip_and_resolve() {
    let dir = std::env::temp_dir().join(format!("guvnor-st-{}", std::process::id()));
    let run = dir.join(".guvnor/runs/123-abc");
    std::fs::create_dir_all(&run).unwrap();
    let st = State::new("123-abc", "t");
    st.save(&run).unwrap();
    let loaded = State::load(&run).unwrap();
    assert_eq!(loaded.status, Status::Planned);
    let resolved = resolve_run_dir(&dir, "123").unwrap();
    assert!(resolved.ends_with("123-abc"));
    assert!(resolve_run_dir(&dir, "zzz").is_err());
    std::fs::remove_dir_all(&dir).ok();
}
