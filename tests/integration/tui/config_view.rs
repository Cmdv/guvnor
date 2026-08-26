use guvnor::tui::{
    cycle_model, screen_text, App, Buttons, ConfigView, LineInput, CFG_ROWS, MODEL_OPTIONS,
    YES_NO,
};
use ratatui::layout::Rect;

/// The config modal is a form, not a list: one blank line between every
/// option, and the ▶ marker (plus the text cursor) still lands on the row
/// it names once those blanks shift everything down.
#[test]
fn config_options_are_blank_separated() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // where each row lands, drawn with `row` selected
    let draw = |row: usize| -> (Vec<String>, u16) {
        let mut app = App::for_test();
        app.config = Some(ConfigView::from_repo(&app.repo));
        app.config.as_mut().unwrap().row = row;
        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
        let lines: Vec<String> =
            screen_text(t.backend().buffer()).lines().map(String::from).collect();
        let at = lines.iter().position(|l| l.contains("language preset")).unwrap() as u16;
        (lines, at)
    };
    let (lines, a) = draw(1); // "test command"
    let b = lines.iter().position(|l| l.contains("test command")).unwrap() as u16;
    assert_eq!(b - a, 2, "one blank line between every option");
    // the ▶ marker opens the row, inside the modal's left border (the row 0
    // value is `◀ node ▶`, so "contains" would lie here)
    let marked = |l: &str| l.split('│').nth(1).is_some_and(|s| s.trim_start().starts_with('▶'));
    assert!(marked(&lines[b as usize]), "the marker follows the selected row");
    assert!(!marked(&lines[a as usize]), "and only that row");
    // stepping onto the action row must not shunt the list: the modal is
    // tall enough for every option, so nothing scrolls.
    assert_eq!(draw(CFG_ROWS - 1).1, a, "the list must not jump on the buttons row");
}

#[test]
fn cycle_model_wraps_and_handles_custom() {
    assert_eq!(cycle_model("opus", 1), "sonnet");
    assert_eq!(cycle_model("opus", -1), MODEL_OPTIONS[MODEL_OPTIONS.len() - 1]);
    assert_eq!(cycle_model("a-hand-edited-model", 1), "opus");
}

#[test]
fn open_drop_keeps_custom_model_first() {
    let mut cv = ConfigView {
        row: 5,
        preset: 0,
        mpreset: 0,
        test: LineInput::default(),
        tests: LineInput::default(),
        src: LineInput::default(),
        models: ["my-custom-model".into(), "sonnet".into(), "opus".into()],
        bin: LineInput::default(),
        timeout: LineInput::default(),
        rework: LineInput::default(),
        drop: None,
        buttons: Buttons::new(&["save", "cancel"], YES_NO),
    };
    cv.open_drop();
    let (sel, options) = cv.drop.as_ref().unwrap();
    assert_eq!(options[0], "my-custom-model");
    assert_eq!(*sel, 0);
    assert_eq!(options.len(), MODEL_OPTIONS.len() + 1);
    // known model: no duplicate entry, selection lands on it
    cv.row = 6;
    cv.open_drop();
    let (sel, options) = cv.drop.as_ref().unwrap();
    assert_eq!(options.len(), MODEL_OPTIONS.len());
    assert_eq!(options[*sel], "sonnet");
}
