use guvnor::digest;
use guvnor::state::{self, State, Status};
use guvnor::tui::{
    click, line_text, next_step, press, screen_text, spec_drifted, spec_sha, status_badge,
    tab_gate, App, CaseView, FAIL_TAB, Go, ReviewFocus, ReviewView, Scroll, Screen, SpecPanels,
    StageView, REVIEW_TAB, TABS,
};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};

/// The hole: an approval has to die with the thing it approved. `replan`
/// resets the tests/work gates (engine side); this is the half that tells
/// you why, so a diff from a superseded spec can't be read as current.
#[test]
fn a_revised_spec_marks_its_old_patches_superseded() {
    let dir = std::env::temp_dir().join(format!("guvnor-stale-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("spec.json"), b"SPEC-V1").unwrap();
    let stale = |st: &State| spec_drifted(spec_sha(&dir).as_deref(), &st.spec_sha_at_run);

    let mut st = State::new("20260101T000000-x", "t");
    assert!(!stale(&st), "nothing run yet is not stale");
    // a run pins the spec its patches came from
    st.spec_sha_at_run = digest::sha256_hex(b"SPEC-V1");
    assert!(!stale(&st));
    // replan rewrites spec.json — the patches now describe the old feature
    std::fs::write(dir.join("spec.json"), b"SPEC-V2").unwrap();
    assert!(stale(&st), "a revised spec must not leave its diffs looking current");
    std::fs::remove_dir_all(&dir).ok();
}

fn view(live: Vec<usize>, shown: Vec<usize>) -> CaseView {
    CaseView {
        id: "x".into(),
        dir: std::path::PathBuf::from("/nonexistent"),
        tab: 0,
        scroll: Scroll::default(),
        note: None,
        feedback: None,
        info: Line::raw(""),
        status: Line::raw(""),
        next: Line::raw(""),
        spec_lines: vec![],
        diffs: Default::default(),
        spec: None,
        panels: SpecPanels::default(),
        approved: [true, true, true],
        review: None,
        review_mark: Span::raw(""),
        live,
        shown,
        tab_cells: Vec::new(),
        fail: None,
        superseded: false,
        confirm: None,
        staged: false,
    }
}

/// `r` on a run that already has patches is a re-run: it bins them and pays
/// for three more lanes, so it asks first, with `cancel` preselected. The
/// first run has nothing to lose and fires straight away.
#[test]
fn a_rerun_asks_first_but_the_first_run_does_not() {
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;
    let asking = |app: &App| match &app.screen {
        Screen::Case(v) => v.confirm.is_some(),
        _ => unreachable!(),
    };
    // nothing run yet: only the Spec tab is live, so `r` just goes
    let mut app = App::for_test();
    app.screen = Screen::Case(Box::new(view(vec![0], vec![0, 1, 2])));
    assert!(
        matches!(app.handle_key(&press(KeyCode::Char('r'))), Some(Go::Run(_))),
        "the first run has nothing to confirm"
    );

    // patches exist: `r` opens the ask instead of firing
    let mut app = App::for_test();
    app.screen = Screen::Case(Box::new(view(vec![0, 1, 2], vec![0, 1, 2])));
    assert!(app.handle_key(&press(KeyCode::Char('r'))).is_none(), "no run yet");
    assert!(asking(&app), "it asks");
    // it says what it costs, and the safe answer is the preselected one
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    t.draw(|f| app.render_case(f, Rect::new(0, 0, 80, 24))).unwrap();
    let screen = screen_text(t.backend().buffer());
    assert!(screen.contains("are replaced"), "the ask says what it costs: {screen:?}");
    // ↵ on the preselected button cancels; the run does not start
    assert!(app.handle_key(&press(KeyCode::Enter)).is_none());
    assert!(!asking(&app), "cancel closes it");

    // and choosing `re-run` is what fires the job
    app.handle_key(&press(KeyCode::Char('r')));
    app.handle_key(&press(KeyCode::Right));
    assert!(
        matches!(app.handle_key(&press(KeyCode::Enter)), Some(Go::Run(_))),
        "→ ↵ is the deliberate answer"
    );
    assert!(!asking(&app), "and the modal closes behind it");
}

/// Landing is the stage box at the foot of the Review tab — `s` from any
/// tab jumps there and focuses it, and the box renders (a full file list +
/// buttons) without panicking, roomy or cramped.
#[test]
fn s_jumps_to_the_stage_box_on_review() {
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;
    let live = vec![0, 1, 2, REVIEW_TAB];
    let mut app = App::for_test();
    let mut v = view(live.clone(), live);
    v.review = Some(Box::new(ReviewView::stub(
        1,
        Some(StageView::build("nope", &Status::Reviewed, None)),
    )));
    v.tab = 2; // on the Work tab: s must reach the box from anywhere
    app.screen = Screen::Case(Box::new(v));

    app.handle_key(&press(KeyCode::Char('s')));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, REVIEW_TAB, "s jumps to the Review tab");
    assert!(
        matches!(v.review.as_deref(), Some(r) if r.focus == ReviewFocus::Stage),
        "and focuses the stage box"
    );

    // the box draws over the Review tab at a roomy and a cramped size —
    // the height math must not panic on a small terminal
    for (w, h) in [(100u16, 30u16), (60, 16)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
    }
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| app.render_case(f, Rect::new(0, 0, 100, 30))).unwrap();
    let screen = screen_text(t.backend().buffer());
    assert!(screen.contains("stage —"), "the box titles itself: {screen:?}");
    assert!(screen.contains("stage"), "and offers the stage action: {screen:?}");
}

/// The strip draws the whole journey, and stepping must visit only the parts
/// of it that have happened — a greyed tab must never be a destination: it
/// looks like a control and answers to nothing.
#[test]
fn stepping_the_strip_only_visits_live_tabs() {
    // failed AND fully approved: Failure is the only conditional tab
    // (landing is the `s` box on the Review tab, not a tab)
    let all = vec![0, 1, 2, REVIEW_TAB, FAIL_TAB];
    let mut v = view(all.clone(), all);
    // forward through every tab and round to the start
    let seen: Vec<usize> = (0..5)
        .map(|_| {
            v.step(1);
            v.tab
        })
        .collect();
    assert_eq!(seen, [1, 2, REVIEW_TAB, FAIL_TAB, 0]);
    // backwards wraps the other way
    v.step(-1);
    assert_eq!(v.tab, FAIL_TAB);
    assert_eq!(v.tab_pos(), 4, "the strip position is not the TABS index");

    // a planned run: the whole journey is drawn, only the spec is enterable
    let mut v = view(vec![0], vec![0, 1, 2, REVIEW_TAB]);
    for d in [1, -1, 1, 1] {
        v.step(d);
        assert_eq!(v.tab, 0, "greyed tabs are not destinations");
    }

    // mid-run: tests exist, work does not — stepping jumps the gap in both
    // directions rather than opening an empty page
    let mut v = view(vec![0, 1], vec![0, 1, 2, REVIEW_TAB]);
    v.step(1);
    assert_eq!(v.tab, 1);
    v.step(1);
    assert_eq!(v.tab, 0, "Work and Review are drawn but not reachable yet");
    v.step(-1);
    assert_eq!(v.tab, 1);
    // ...and the strip position still tracks what is drawn, not what is live
    assert_eq!(v.tab_pos(), 1);
}

/// `tab` used to double as a third way to step the strip, alongside ←/→ and
/// h/l. That meant it did two different things depending on the screen — the
/// strip moves on ←/→ (h/l) only now, and tab is free for focus everywhere,
/// even on a tab (like Tests) with no boxes to focus, where it is simply a
/// no-op rather than a hidden way to change tabs.
#[test]
fn tab_never_steps_the_strip_only_arrows_do() {
    use ratatui::crossterm::event::KeyCode;
    let mut app = App::for_test();
    app.screen = Screen::Case(Box::new(view(vec![0, 1, 2], vec![0, 1, 2])));
    if let Screen::Case(v) = &mut app.screen {
        v.tab = 1;
    }
    app.handle_key(&press(KeyCode::Tab));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, 1, "tab must not move the strip");

    app.handle_key(&press(KeyCode::BackTab));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, 1, "backtab must not either");

    app.handle_key(&press(KeyCode::Right));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, 2, "←/→ still does");
}

/// The strip draws the whole journey, so it must say which parts of it are
/// reachable — a dim label is a promise, a bright one is a control.
#[test]
fn the_strip_draws_the_whole_journey_and_greys_what_has_not_happened() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    // a freshly planned run: nothing but a spec
    app.screen = Screen::Case(Box::new(view(vec![0], vec![0, 1, 2, REVIEW_TAB])));
    let (w, h) = (100, 20);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
    let buf = t.backend().buffer().clone();
    let cells: Vec<String> = (0..w).map(|x| buf[(x, 1)].symbol().to_string()).collect();
    let row = cells.concat();
    // the whole journey is named from the start — that is the map
    for label in ["Spec", "Tests", "Work", "Review"] {
        assert!(row.contains(label), "{label} missing from the strip: {row}");
    }
    // Failure is not a stage of the journey, so it is not promised; landing
    // is the `s` box on the Review tab, not a tab of its own
    assert!(!row.contains("Failure"), "a run that hasn't failed must not offer it");
    assert!(!row.contains("Stage"), "landing is the s box on Review, not a tab");
    // by cell, not by byte: the row is mostly multi-byte glyphs
    let fg_at = |needle: &str| {
        let x = (0..w).find(|&x| cells[x as usize..].concat().starts_with(needle)).unwrap();
        buf[(x, 1)].style().fg
    };
    assert_eq!(fg_at("Tests"), Some(Color::DarkGray), "unreachable tabs must read as later");
    assert_eq!(fg_at("Work"), Some(Color::DarkGray));
    assert_ne!(fg_at("Spec"), Some(Color::DarkGray), "the tab you are on is not greyed");
}

/// The status goes hard right, the run's name stays left, and the
/// message sits in the gap between them. A chip whose position depends on
/// the length of the title is a chip you have to hunt for.
#[test]
fn name_left_message_in_the_gap_status_hard_right() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::for_test();
    let mut v = view(vec![0], vec![0, 1, 2, REVIEW_TAB]);
    v.info = Line::from(Span::raw("more math functions"));
    v.status = Line::from(vec![status_badge(&state::Status::Reviewed), Span::raw(" ")]);
    v.next = Line::from(vec![
        Span::raw(" ▸ "),
        Span::raw("c"),
        Span::raw(" every gate is green"),
    ]);
    app.screen = Screen::Case(Box::new(v));
    let (w, h) = (160, 20);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
    let buf = t.backend().buffer().clone();
    let cells: Vec<String> = (0..w).map(|x| buf[(x, 1)].symbol().to_string()).collect();
    let x_of = |needle: &str| {
        (0..w).find(|&x| cells[x as usize..].concat().starts_with(needle)).unwrap()
    };
    // the chip's fill reaches the last usable column, whatever the title is
    assert_eq!(buf[(w - 2, 1)].style().bg, Some(Color::Cyan), "status is not flush right");
    // all three on one row, in that order
    assert!(x_of("more math functions") < x_of("every gate is green"), "{}", cells.concat());
    assert!(x_of("every gate is green") < x_of("reviewed"), "{}", cells.concat());
}

/// Whatever state a run is in, the screen says the next move and names the
/// key.
#[test]
fn the_next_move_is_always_on_screen() {
    let text = line_text;
    let key = |l: &Line| l.spans[1].content.to_string();
    let gates = |s, t, w| {
        let mut g = state::Gates::default();
        g.spec.approved = s;
        g.tests.approved = t;
        g.work.approved = w;
        g
    };
    let go = state::Status::SpecApproved;

    // unapproved: approving is the only move, and it is ↵
    let l = next_step(&gates(false, false, false), &go, false, false, false);
    assert_eq!(key(&l), "↵");
    assert!(text(&l).contains("approve"), "{}", text(&l));

    // approved and never run — THE reported gap
    let l = next_step(&gates(true, false, false), &go, false, false, false);
    assert_eq!(key(&l), "r");
    assert!(text(&l).contains("run the lanes"), "{}", text(&l));

    // run done: judge the tests, then the work, then land it
    let l = next_step(&gates(true, false, false), &Status::Reviewed, false, false, true);
    assert!(text(&l).contains("Tests"), "{}", text(&l));
    let l = next_step(&gates(true, true, false), &Status::Reviewed, false, false, true);
    assert!(text(&l).contains("Work"), "{}", text(&l));
    let l = next_step(&gates(true, true, true), &Status::Reviewed, false, false, true);
    assert!(text(&l).contains("stage"), "{}", text(&l));
    assert!(
        l.spans.iter().any(|s| s.content == "s" && s.style.fg == Some(Color::Red)),
        "the s hotkey is red, embedded in the sentence: {}",
        text(&l)
    );
    let l = next_step(&gates(true, true, true), &Status::Staged, false, false, true);
    assert!(text(&l).contains("staged in your tree"), "{}", text(&l));
    assert!(text(&l).contains("commit"), "{}", text(&l));
    assert!(
        l.spans.iter().any(|s| s.content == "s" && s.style.fg == Some(Color::Red)),
        "the s in 'staged' is the red hotkey: {}",
        text(&l)
    );

    // an edited spec outranks everything short of a break: the approval on
    // record is for different words
    let l = next_step(&gates(true, true, true), &Status::Reviewed, true, false, true);
    assert!(text(&l).contains("changed since you approved"), "{}", text(&l));
    // ...and a break outranks that
    let broke = Status::Failed("vacuous_tests".into());
    let l = next_step(&gates(true, true, true), &broke, true, false, true);
    assert!(text(&l).contains("Failure tab"), "{}", text(&l));
    // a rejection is a decision, not a break — it must not claim a Failure tab
    let no = Status::Failed("rejected_work".into());
    let l = next_step(&gates(false, false, false), &no, false, false, false);
    assert!(text(&l).contains("approve"), "{}", text(&l));
    // done is done: no key to press, nothing left to do
    let l = next_step(&gates(true, true, true), &Status::Committed, false, false, true);
    assert!(text(&l).contains("committed"), "{}", text(&l));
    assert!(!text(&l).contains('▸'), "nothing to press: {}", text(&l));
}

#[test]
fn tab_maps_to_its_gate() {
    // a wrong mapping here silently approves the wrong gate — assert all
    // three, and that the tabs without one are the reports (landing is not a
    // tab: it is the `s` box on Review)
    assert_eq!(TABS.len(), 5);
    assert_eq!(tab_gate(0).as_str(), "spec");
    assert_eq!(tab_gate(1).as_str(), "tests");
    assert_eq!(tab_gate(2).as_str(), "work");
    assert_eq!(TABS[REVIEW_TAB], "Review");
    assert_eq!(TABS[FAIL_TAB], "Failure");
    // the gate array is indexed by tab: the rest must sit past its end
    assert_eq!(REVIEW_TAB, 3);
    assert_eq!(FAIL_TAB, TABS.len() - 1);
}

/// A click hits exactly what render drew: `tab_cells` is that same
/// geometry, not a guess from label widths. A tab that hasn't happened
/// yet is a no-op, same as `step`'s keyboard move.
#[test]
fn clicking_a_tab_selects_it_and_a_greyed_one_is_a_no_op() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // Spec and Tests exist; Work and Review are drawn but not live yet.
    let mut app = App::for_test();
    app.screen = Screen::Case(Box::new(view(vec![0, 1], vec![0, 1, 2, REVIEW_TAB])));
    let (w, h) = (100, 24);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab_cells.len(), 4, "one cell per shown tab");
    let (tests_cell, work_cell) = (v.tab_cells[1], v.tab_cells[2]);

    // clicking the live "Tests" cell selects it
    app.handle_mouse(&click(tests_cell.x + 1, tests_cell.y + 1));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, 1, "the click landed on Tests");

    // clicking the greyed "Work" cell does nothing. Never a destination.
    app.handle_mouse(&click(work_cell.x + 1, work_cell.y + 1));
    let Screen::Case(v) = &app.screen else { unreachable!() };
    assert_eq!(v.tab, 1, "a greyed tab is not a click target either");
}
