use guvnor::tui::{
    click, cycle_model, screen_text, App, Buttons, Cells, ConfigView, LineInput, CFG_ROWS,
    MODEL_OPTIONS, YES_NO,
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

/// A click on a settings row focuses it — the same landing spot ↓/↑ give.
#[test]
fn clicking_a_settings_row_focuses_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    app.config = Some(ConfigView::from_repo(&app.repo));
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
    let cell = app.config.as_ref().unwrap().row_cell(8).expect("claude bin row drew somewhere");

    assert!(app.handle_mouse(&click(cell.x + 1, cell.y)).is_none());
    assert_eq!(app.config.as_ref().unwrap().row, 8);
}

/// The preset rows' ◀/▶ are narrower than the row: a click there cycles
/// immediately instead of only focusing, same as → would once landed on it.
#[test]
fn clicking_a_presets_arrow_cycles_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    app.config = Some(ConfigView::from_repo(&app.repo));
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
    let right = app.config.as_ref().unwrap().preset_cell(0, 1).expect("▶ drew somewhere");

    assert!(app.handle_mouse(&click(right.x, right.y)).is_none());
    let cv = app.config.as_ref().unwrap();
    assert_eq!(cv.row, 0);
    assert_eq!(cv.preset, 1, "the click cycled forward, same as →");
}

/// A click on a model seat opens its dropdown, same as landing there and
/// pressing ↵ would.
#[test]
fn clicking_a_model_seat_opens_its_dropdown() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    app.config = Some(ConfigView::from_repo(&app.repo));
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
    let cell = app.config.as_ref().unwrap().row_cell(6).expect("model worker row drew somewhere");

    assert!(app.handle_mouse(&click(cell.x + 1, cell.y)).is_none());
    let cv = app.config.as_ref().unwrap();
    assert_eq!(cv.row, 6);
    assert!(cv.drop.is_some(), "the click opened the dropdown");
}

/// A click on a dropdown option picks it and closes the dropdown, same as
/// ↑↓ then ↵ would.
#[test]
fn clicking_a_dropdown_option_picks_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    app.config = Some(ConfigView::from_repo(&app.repo));
    let cv = app.config.as_mut().unwrap();
    cv.row = 6;
    cv.open_drop();
    let want = cv.drop.as_ref().unwrap().1[2].clone();
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
    let cell = app.config.as_ref().unwrap().drop_cell(2).expect("an option drew somewhere");

    assert!(app.handle_mouse(&click(cell.x + 1, cell.y)).is_none());
    let cv = app.config.as_ref().unwrap();
    assert!(cv.drop.is_none(), "picking an option closes the dropdown");
    assert_eq!(cv.models[1], want, "the clicked option was picked");
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
        row_cells: Cells::default(),
        preset_cells: Cells::default(),
        drop_cells: Cells::default(),
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
