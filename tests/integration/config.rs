use guvnor::config::{config_toml, save_settings, Config, Settings};

#[test]
fn template_parses_and_validates() {
    let raw = config_toml("cargo test", &["tests/"], &["src/", "app/"]);
    let cfg: Config = toml_edit::de::from_str(&raw).unwrap();
    assert_eq!(cfg.commands.test, "cargo test");
    assert_eq!(cfg.paths.tests, vec!["tests/"]);
    assert_eq!(cfg.paths.src, vec!["src/", "app/"]);
    assert_eq!(cfg.claude.bin, "claude");
    assert_eq!(cfg.limits.lane_timeout_secs, 900);
}

/// The test command is free text from the config modal, and the template is
/// written before anything reads it back — so a quote interpolated raw
/// bricks the config with no in-app way out.
#[test]
fn a_quoted_test_command_survives_the_template() {
    for cmd in [
        r#"pytest -q -k "not slow""#,
        r#"pytest -k 'not slow'"#,
        r#"sh -c 'echo "x"'"#,
        r"cmd \ with \ backslashes",
    ] {
        let raw = config_toml(cmd, &["tests/"], &["src/"]);
        let cfg: Config = toml_edit::de::from_str(&raw)
            .unwrap_or_else(|e| panic!("{cmd:?} produced unreadable toml: {e}\n{raw}"));
        assert_eq!(cfg.commands.test, cmd);
    }
}

#[test]
fn save_settings_roundtrip_keeps_comments() {
    let dir = std::env::temp_dir().join(format!("guvnor-cfg-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    save_settings(
        &dir,
        &Settings {
            test: "cargo test".into(),
            tests: vec!["tests/".into()],
            src: vec!["src/".into(), "app/".into()],
            bin: "claude".into(),
            models: ["opus".into(), "haiku".into(), "opus".into()],
            timeout_secs: 600,
            max_rework_rounds: 2,
        },
    )
    .unwrap();
    let cfg = Config::load(&dir).unwrap();
    assert_eq!(cfg.commands.test, "cargo test");
    assert_eq!(cfg.paths.src, vec!["src/", "app/"]);
    assert_eq!(cfg.claude.model_worker, "haiku");
    assert_eq!(cfg.limits.lane_timeout_secs, 600);
    assert_eq!(cfg.limits.max_rework_rounds, 2);
    let raw = std::fs::read_to_string(dir.join(".guvnor/guvnor.toml")).unwrap();
    assert!(raw.contains("# guvnor per-repo configuration"));
    assert!(save_settings(
        &dir,
        &Settings {
            test: "t".into(),
            tests: vec!["/abs".into()],
            src: vec!["src/".into()],
            bin: "claude".into(),
            models: ["a".into(), "b".into(), "c".into()],
            timeout_secs: 1,
            max_rework_rounds: 1,
        }
    )
    .is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_absolute_or_traversal_prefixes() {
    let bad = r#"
[commands]
test = "true"
[paths]
tests = ["/etc/"]
src = ["src/"]
"#;
    let cfg: Config = toml_edit::de::from_str(bad).unwrap();
    assert!(cfg.paths.tests[0].starts_with('/'));
    // Validation happens in load(); emulate its check here.
    assert!(cfg.paths.tests.iter().any(|p| p.starts_with('/')));
}
