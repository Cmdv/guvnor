use guvnor::state;
use guvnor::tui::runs::{HomeFocus, RunRow};
use guvnor::tui::{gates_line, press, screen_text, App, Go, ART_WHITE, JobKind, SELECTED_TEXT};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Color;

/// The new-feature box has no action row: ↵ plans it from either field, and
/// the only way to get a newline into the context is ⇧↵. It's the focused
/// box on the home screen, not a separate screen.
#[test]
fn enter_plans_it_and_shift_enter_types_a_newline() {
    let dir = std::env::temp_dir().join(format!("guvnor-newkeys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = App::new(dir.clone(), false);
    app.config = None; // an uninitialised repo greets you with the config modal
    app.focus = HomeFocus::New; // tab off the runs list onto the panel
    let key = |code, m| KeyEvent::new(code, m);
    let typed = |app: &mut App, s: &str| {
        for c in s.chars() {
            app.handle_key(&key(KeyCode::Char(c), KeyModifiers::NONE));
        }
    };

    // no title, no job — and it says so instead of planning nothing
    assert!(app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)).is_none());
    assert!(app.toast.is_some());

    typed(&mut app, "add stats");
    app.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE));
    typed(&mut app, "one");
    app.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
    typed(&mut app, "two");
    match app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)) {
        Some(Go::Plan(title, context)) => {
            assert_eq!(title, "add stats");
            assert_eq!(context, "one\ntwo", "⇧↵ is the newline, ↵ is the submit");
        }
        other => panic!("↵ should have planned it, got {}", other.is_none()),
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The new-feature panel is a permanent box on the home screen — no key
/// press, no separate screen. Its two inner fields are always drawn.
#[test]
fn the_panel_is_always_on_the_home_screen() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    let mut t = Terminal::new(TestBackend::new(120, 50)).unwrap();
    t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 50))).unwrap();
    let screen = screen_text(t.backend().buffer());
    assert!(screen.contains("feature title"), "the title field is always drawn: {screen:?}");
    assert!(screen.contains("context for the planner"), "and the context field");
    // the responsive art tiers (full / letters-only / none) must not panic
    // the 50/50 split on a cramped screen
    for (w, h) in [(120u16, 50u16), (100, 30), (40, 14)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render_runs(f, Rect::new(0, 0, w, h))).unwrap();
    }
}

/// A panic inside `draw` escapes before the terminal is handed back, so a
/// narrow window is not a cosmetic problem: it leaves the shell in raw mode.
/// The delete popup wants 38 columns and has to cope with fewer.
#[test]
fn the_delete_popup_survives_a_terminal_narrower_than_it_wants() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    app.runs = vec![RunRow {
        id: "id-1".into(),
        title: "a title long enough to want more room than this".into(),
        status: state::Status::Staged,
        verdict: String::new(),
        cost: String::new(),
        gates: gates_line(&guvnor::state::Gates::default()),
    }];
    app.table.select(Some(0));
    app.handle_key(&press(KeyCode::Char('d')));
    assert!(app.confirm_delete.is_some());
    // the whole dispatch, so the popup gets the inner rect it gets for real
    for w in [10u16, 20, 39, 40, 120] {
        let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
        t.draw(|f| app.render(f)).unwrap();
    }
}

/// Tab walks the keyboard Runs → title → context → Runs; esc from the panel
/// hands it straight back. The runs list holds focus at startup.
#[test]
fn tab_cycles_focus_between_runs_and_the_panel() {
    let mut app = App::for_test();
    assert!(app.focus == HomeFocus::Runs, "home starts on the runs list");
    app.handle_key(&press(KeyCode::Tab));
    assert!(
        app.focus == HomeFocus::New && app.new.focus == 0,
        "tab enters the panel at the title"
    );
    app.handle_key(&press(KeyCode::Tab));
    assert!(app.focus == HomeFocus::New && app.new.focus == 1, "then the context");
    app.handle_key(&press(KeyCode::Tab));
    assert!(app.focus == HomeFocus::Runs, "then back to the runs list");
    app.handle_key(&press(KeyCode::Tab)); // onto the panel again
    app.handle_key(&press(KeyCode::Esc));
    assert!(app.focus == HomeFocus::Runs, "esc hands focus back to the list");
}

/// Selecting a row must leave the coloured chips alone — not just green, but
/// every badge colour (the cyan `reviewed` here) — and lay a plain bar over
/// the rest of the row.
#[test]
fn selecting_a_row_keeps_badge_colours_and_bars_the_rest() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let row = |status: state::Status| RunRow {
        id: format!("id-{status}"),
        title: "a feature".into(),
        status,
        verdict: "APPROVED".into(),
        cost: "$1.00".into(),
        gates: gates_line(&guvnor::state::Gates::default()),
    };
    let mut app = App::for_test();
    app.runs = vec![row(state::Status::Reviewed), row(state::Status::Committed)]; // cyan, green
    app.table.select(Some(0)); // the cyan `reviewed` row is the selected one
    let mut t = Terminal::new(TestBackend::new(120, 20)).unwrap();
    t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 20))).unwrap();
    let buf = t.backend().buffer().clone();
    let cell = |want: Color| {
        (0..20)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].clone())
            .find(|c| c.style().bg == Some(want))
    };
    // the cyan reviewed chip is on the selected row: it keeps its cyan
    // background but its letters go the darker selected-text grey to read
    // as selected.
    let cyan = cell(Color::Cyan).expect("the reviewed chip keeps its cyan background");
    assert_eq!(cyan.style().fg, Some(SELECTED_TEXT), "selected chip gets the darker grey letters");
    assert!(cell(Color::Green).is_some(), "other badges keep their colour too");
    assert!(cell(ART_WHITE).is_some(), "the selected row wears the plain bar");
}

/// The gates column on a selected row must render as one grey: approved
/// (green-backed) chips, unapproved `·` chips, and the `│` dividers between
/// them all take the same darker-than-the-bar text colour — only the
/// approved chip's green fill survives.
#[test]
fn selected_row_gates_take_selected_text_and_keep_chip_backgrounds() {
    use guvnor::state::{Approval, Gates};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let gates = Gates {
        spec: Approval { approved: true, ..Default::default() },
        tests: Approval::default(),
        work: Approval::default(),
    };
    let mut app = App::for_test();
    app.runs = vec![RunRow {
        id: "id-1".into(),
        title: "a feature".into(),
        // cyan, not green, so the approved gate chip's green background
        // can't be confused with the status badge's own fill.
        status: state::Status::Reviewed,
        verdict: "APPROVED".into(),
        cost: "$1.00".into(),
        gates: gates_line(&gates),
    }];
    app.table.select(Some(0));
    let mut t = Terminal::new(TestBackend::new(120, 20)).unwrap();
    t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 20))).unwrap();
    let buf = t.backend().buffer().clone();
    let cells = || (0..20).flat_map(|y| (0..120).map(move |x| (x, y))).map(|(x, y)| buf[(x, y)].clone());

    let approved_chip = cells()
        .find(|c| c.style().bg == Some(Color::Green))
        .expect("the approved gate chip keeps its green background");
    assert_eq!(
        approved_chip.style().fg,
        Some(SELECTED_TEXT),
        "approved chip letters go the darker selected-text grey"
    );

    let unapproved = cells().find(|c| c.symbol() == "·").expect("an unapproved chip dot is drawn");
    assert_eq!(
        unapproved.style().fg,
        Some(SELECTED_TEXT),
        "unapproved chip dots go the darker selected-text grey too"
    );

    let divider = cells().find(|c| c.symbol() == "│" && c.style().fg == Some(SELECTED_TEXT));
    assert!(divider.is_some(), "the gate dividers also take the darker selected-text grey");
}

/// Off the bar, nothing about the status badge or the gates line changes:
/// approved chips stay black-on-colour, unapproved chips and dividers stay
/// plain dark grey.
#[test]
fn unselected_row_status_and_gates_keep_their_original_colours() {
    use guvnor::state::{Approval, Gates};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let gates = Gates {
        spec: Approval { approved: true, ..Default::default() },
        tests: Approval::default(),
        work: Approval::default(),
    };
    let mut app = App::for_test();
    app.runs = vec![RunRow {
        id: "id-1".into(),
        title: "a feature".into(),
        status: state::Status::Committed,
        verdict: "APPROVED".into(),
        cost: "$1.00".into(),
        gates: gates_line(&gates),
    }];
    app.table.select(None); // nothing selected — the row is plain
    let mut t = Terminal::new(TestBackend::new(120, 20)).unwrap();
    t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 20))).unwrap();
    let buf = t.backend().buffer().clone();
    let cells = || (0..20).flat_map(|y| (0..120).map(move |x| (x, y))).map(|(x, y)| buf[(x, y)].clone());

    let greens: Vec<_> = cells().filter(|c| c.style().bg == Some(Color::Green)).collect();
    assert!(!greens.is_empty(), "the committed badge and the approved chip both render green");
    assert!(
        greens.iter().all(|c| c.style().fg == Some(Color::Black)),
        "their letters stay black off the bar"
    );

    let unapproved = cells().find(|c| c.symbol() == "·").expect("an unapproved chip dot is drawn");
    assert_eq!(unapproved.style().fg, Some(Color::DarkGray), "unapproved chips stay dark grey off the bar");

    let divider = cells().find(|c| c.symbol() == "│" && c.style().fg == Some(Color::DarkGray));
    assert!(divider.is_some(), "dividers stay dark grey off the bar");
}

/// A run's spinner status is the one status span with no background of its
/// own — it must still pick up the darker selected-text grey when its row is
/// selected, and stay plain cyan when it isn't.
#[test]
fn a_running_rows_spinner_takes_selected_text_when_selected_and_cyan_otherwise() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let make_app = |selected: bool| {
        let mut app = App::for_test();
        app.runs = vec![RunRow {
            id: "id-1".into(),
            title: "a feature".into(),
            status: state::Status::Planned,
            verdict: String::new(),
            cost: String::new(),
            gates: gates_line(&guvnor::state::Gates::default()),
        }];
        app.table.select(if selected { Some(0) } else { None });
        app.start_job(JobKind::Run, Some("id-1".into()), |_tx| Ok(0));
        app
    };
    let running_style = |app: &mut App| {
        let mut t = Terminal::new(TestBackend::new(120, 20)).unwrap();
        t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 20))).unwrap();
        let screen = screen_text(t.backend().buffer());
        let (y, line) = screen
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("running"))
            .expect("the running row is drawn");
        let x = line.find("running").unwrap();
        t.backend().buffer()[(x as u16, y as u16)].style()
    };

    let mut selected = make_app(true);
    assert_eq!(
        running_style(&mut selected).fg,
        Some(SELECTED_TEXT),
        "the selected running row's spinner text goes the darker selected-text grey too"
    );

    let mut unselected = make_app(false);
    assert_eq!(running_style(&mut unselected).fg, Some(Color::Cyan), "off the bar it stays cyan");
}

/// The theme constant itself: an exact, achromatic grey strictly darker than
/// the selection bar's own fill, tuned to stay legible against it.
#[test]
fn selected_text_is_a_darker_achromatic_tone_of_the_bar() {
    match SELECTED_TEXT {
        Color::Rgb(r, g, b) => {
            assert_eq!(r, g, "SELECTED_TEXT must be achromatic (r == g)");
            assert_eq!(g, b, "SELECTED_TEXT must be achromatic (g == b)");
            assert!(r < 0xea, "SELECTED_TEXT must be strictly darker than ART_WHITE (0xea)");
            assert!((0x70..=0xb0).contains(&r), "SELECTED_TEXT must stay in the legible 0x70-0xb0 range");
        }
        other => panic!("SELECTED_TEXT must be an exact Color::Rgb value, got {other:?}"),
    }
}

/// `d` bins any run you haven't landed — planned, failed, staged alike.
/// The one exception is a committed run: its evidence is the record behind
/// a commit that already exists.
#[test]
fn d_deletes_anything_but_a_committed_run() {
    use state::Status;
    let row = |status: Status| RunRow {
        id: format!("id-{status}"),
        title: status.to_string(),
        status,
        verdict: String::new(),
        cost: String::new(),
        gates: gates_line(&guvnor::state::Gates::default()),
    };
    for status in
        [Status::Planned, Status::Reviewed, Status::Staged, Status::Failed("vacuous_tests".into())]
    {
        let mut app = App::for_test();
        app.runs = vec![row(status.clone())];
        app.table.select(Some(0));
        app.handle_key(&press(KeyCode::Char('d')));
        assert!(app.confirm_delete.is_some(), "{status} must be deletable");
    }
    let mut app = App::for_test();
    app.runs = vec![row(Status::Committed)];
    app.table.select(Some(0));
    app.handle_key(&press(KeyCode::Char('d')));
    assert!(app.confirm_delete.is_none(), "a committed run is the record — no delete");
    assert!(app.toast.is_some(), "and it says why");
}
