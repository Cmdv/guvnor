use guvnor::tui::review::{render_cost, render_review_tab, review_key, ReviewFocus, ReviewView, Took, COST_W};
use guvnor::tui::{press, screen_text, Go};
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::text::Line;

/// A `ReviewView` with `n` findings and no stage box — enough to drive keys.
fn review_stub(n: usize) -> ReviewView {
    ReviewView::stub(n, None)
}


#[test]
fn review_tab_takes_its_own_keys_but_never_traps_the_tab_strip() {
    let mut r = review_stub(2);
    // ←/→ are the run screen's tab keys everywhere else: on a finding row
    // the review must hand them back, or Review becomes a tab you can't leave
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char('h'), KeyCode::Char('l')] {
        assert!(matches!(review_key(&mut r, &press(code)), Took::No), "{code:?} must pass through");
    }
    // any unbound letter passes through
    assert!(matches!(review_key(&mut r, &press(KeyCode::Char('m'))), Took::No));

    // ↵ on a finding ticks it
    assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Yes));
    assert!(r.checked[0]);

    // the action row: nothing ticked and nothing typed says so instead of
    // launching an empty job
    r.sel = r.action_row();
    r.checked[0] = false;
    assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Say(_)));
    r.checked[0] = true;
    assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Go(Go::Fix(..))));

    // an instruction the implementer can't act on goes to the planner —
    // the second button is the only way out of "CANNOT: contradicts the spec"
    review_key(&mut r, &press(KeyCode::Down)); // ↓ walks the row, not ←/→
    assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Say(_)), "no words to send");
    r.note.value = "drop the LICENSE file".into();
    assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Go(Go::Replan(..))));
    // ...and ←/→ on the action row still belong to the tab strip
    assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::No));
    r.note.value.clear();

    // on the instruction line letters type instead of moving the cursor
    r.sel = r.note_row();
    review_key(&mut r, &press(KeyCode::Char('j')));
    assert_eq!(r.note.value, "j");
    assert_eq!(r.sel, r.note_row(), "typing must not move off the field");
    // ←/→ move the text cursor only while there is text: an empty field is
    // not worth trapping the tab keys for
    assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::Yes));
    r.note.value.clear();
    assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::No));
    review_key(&mut r, &press(KeyCode::Down));
    assert_eq!(r.sel, r.action_row(), "the bare arrow is the way out");

    // off the findings, the arrows scroll the pane `tab` selected
    r.focus = ReviewFocus::Cost;
    r.cost_scroll.max.set(9);
    r.note.value = "keep me".into();
    review_key(&mut r, &press(KeyCode::Down));
    assert_eq!(r.cost_scroll.off, 1);
    assert_eq!(r.note.value, "keep me", "scrolling must not reach the text field");
}

/// Reported: on a review with no findings the cursor used to start on the
/// instruction line (coincidentally `note_row() == 0` too), handing the
/// keyboard to a text field uninvited. It now starts on the actions instead,
/// and ←/→ must still reach the tab strip from there.
#[test]
fn an_empty_review_is_not_a_tab_you_get_stuck_in() {
    let mut r = review_stub(0);
    assert_eq!(r.sel, r.action_row(), "nothing to tick or type: land on the actions, not the field");
    for code in [KeyCode::Left, KeyCode::Right] {
        assert!(
            matches!(review_key(&mut r, &press(code)), Took::No),
            "{code:?} must reach the tab strip"
        );
    }
    // and from the action row
    r.sel = r.action_row();
    assert!(matches!(review_key(&mut r, &press(KeyCode::Right)), Took::No));
    // ↓ is what walks the buttons
    assert!(matches!(review_key(&mut r, &press(KeyCode::Down)), Took::Yes));
    assert_eq!(r.buttons.sel, 1);
}

#[test]
fn cost_header_and_total_stay_put_while_the_ledger_scrolls() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut v = review_stub(0);
    v.cost = (0..20)
        .map(|i| guvnor::casefile::CostRow {
            name: format!("lane{i}"),
            tin: 1000,
            tout: 100,
            cost: 0.01,
        })
        .collect();
    v.cost_total = guvnor::casefile::cost_total(&v.cost);
    // 8 rows: border, blank (the box's lead line), header, 3 body rows,
    // footer, border
    let mut t = Terminal::new(TestBackend::new(COST_W, 8)).unwrap();
    let area = Rect::new(0, 0, COST_W, 8);
    let row = |b: &ratatui::buffer::Buffer, y: u16| {
        screen_text(b).lines().nth(y as usize).unwrap().to_string()
    };

    t.draw(|f| render_cost(f, area, &v)).unwrap();
    let top = row(t.backend().buffer(), 2);
    let bottom = row(t.backend().buffer(), 6);
    assert!(top.contains("lane") && top.contains("in") && top.contains("out"), "{top:?}");
    // 20 lanes at $0.01 — the total is on screen without scrolling to it
    assert!(bottom.contains("total") && bottom.contains("$0.20"), "{bottom:?}");
    assert!(row(t.backend().buffer(), 3).contains("lane0"));
    // no per-row `tok`: the unit is in the heading
    assert!(!row(t.backend().buffer(), 3).contains("tok"));

    v.cost_scroll.max.set(16);
    v.cost_scroll.by(16);
    t.draw(|f| render_cost(f, area, &v)).unwrap();
    assert_eq!(row(t.backend().buffer(), 2), top, "header scrolled away");
    assert_eq!(row(t.backend().buffer(), 6), bottom, "total scrolled away");
    assert!(row(t.backend().buffer(), 3).contains("lane16"), "body did not scroll");
}

#[test]
fn a_pane_scrolls_until_its_last_line_rests_on_the_bottom_row() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut v = review_stub(0);
    v.focus = ReviewFocus::Summary;
    v.summary = (0..30).map(|i| Line::raw(format!("line{i:02}"))).collect();
    let area = Rect::new(0, 0, 100, 22);
    let mut t = Terminal::new(TestBackend::new(100, 22)).unwrap();
    // which prose lines are on screen, top to bottom — layout-independent,
    // so this keeps testing the scroll and not the box sizes
    let shown = |t: &Terminal<TestBackend>| -> Vec<u32> {
        screen_text(t.backend().buffer())
            .lines()
            .filter_map(|r| r.find("line").map(|i| r[i + 4..i + 6].parse().unwrap()))
            .collect()
    };

    t.draw(|f| render_review_tab(f, area, &v)).unwrap();
    let first = shown(&t);
    assert_eq!(first.first(), Some(&0), "should start at the top: {first:?}");
    assert!(first.len() >= 3, "pane too small to test: {first:?}");

    // hold ↓ down: it must stop with line29 on the bottom row, never scroll
    // the text off into an empty box
    for _ in 0..500 {
        v.summary_scroll.by(1);
        t.draw(|f| render_review_tab(f, area, &v)).unwrap();
    }
    let last = shown(&t);
    assert_eq!(last.last(), Some(&29), "last line must rest on the bottom: {last:?}");
    assert_eq!(last.len(), first.len(), "same screenful, just scrolled");
}

/// A number in a box title jumps and zooms straight to it, Spec-tab style.
#[test]
fn a_number_jumps_and_zooms_its_section() {
    let mut r = review_stub(2);
    for (key, want) in [
        ('2', ReviewFocus::Summary),
        ('3', ReviewFocus::Cost),
        ('4', ReviewFocus::Stage),
        ('1', ReviewFocus::Findings),
    ] {
        r.zoomed = false;
        assert!(matches!(review_key(&mut r, &press(KeyCode::Char(key))), Took::Yes));
        assert!(r.focus == want, "{key} should have jumped");
        assert!(r.zoomed, "{key} should have zoomed");
    }
    // ...and it works from anywhere, not just the findings
    r.focus = ReviewFocus::Cost;
    review_key(&mut r, &press(KeyCode::Char('2')));
    assert!(r.focus == ReviewFocus::Summary);

    // but never while typing an instruction: the digits are text there
    r.focus = ReviewFocus::Findings;
    r.zoomed = false;
    r.sel = r.note_row();
    review_key(&mut r, &press(KeyCode::Char('3')));
    assert_eq!(r.note.value, "3");
    assert!(r.focus == ReviewFocus::Findings);
    assert!(!r.zoomed, "typing a digit must not zoom");
    // the buttons hold no digits, so the jumps keep working while the
    // cursor is on them — `→ ↵` is how you pick the second one
    r.focus = ReviewFocus::Findings;
    r.sel = r.action_row();
    r.note.value = "drop it".into();
    assert!(matches!(review_key(&mut r, &press(KeyCode::Char('2'))), Took::Yes));
    assert!(r.focus == ReviewFocus::Summary);
}

/// ↓ must not dead-end on the first button; it walks the whole action row.
#[test]
fn down_walks_the_buttons_and_comes_back_round_to_the_list() {
    let mut r = review_stub(2);
    for _ in 0..3 {
        review_key(&mut r, &press(KeyCode::Down)); // findings → note → actions
    }
    assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 0));
    review_key(&mut r, &press(KeyCode::Down));
    assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 1), "↓ steps along the row");
    review_key(&mut r, &press(KeyCode::Up));
    assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 0), "↑ steps back");
    review_key(&mut r, &press(KeyCode::Down));
    review_key(&mut r, &press(KeyCode::Down));
    assert_eq!((r.sel, r.buttons.sel), (0, 0), "off the end is the top of the list");
    // and ↑ off the first button still lands on the instruction line
    r.sel = r.action_row();
    review_key(&mut r, &press(KeyCode::Up));
    assert_eq!(r.sel, r.note_row());
}

/// Zoom is the point of the number: it doesn't just move the cursor, it
/// makes that pane the only one on the tab, tmux style.
#[test]
fn zooming_a_pane_hides_the_others() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut v = review_stub(1);
    v.zoomed = true;
    v.focus = ReviewFocus::Cost;
    v.cost = vec![guvnor::casefile::CostRow { name: "planner".into(), tin: 10, tout: 10, cost: 0.01 }];
    v.cost_total = guvnor::casefile::cost_total(&v.cost);
    let area = Rect::new(0, 0, 100, 30);
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();

    t.draw(|f| render_review_tab(f, area, &v)).unwrap();
    let text = screen_text(t.backend().buffer());
    assert!(text.contains("3 tokens / cost"), "{text}");
    assert!(!text.contains("1 findings"), "the rest must be gone, not just unfocused:\n{text}");
    assert!(!text.contains("2 reviewer comment"), "{text}");
    assert!(!text.contains("4 stage"), "{text}");
    assert!(text.contains("unzoom"), "the way back belongs on screen: {text}");

    v.zoomed = false;
    t.draw(|f| render_review_tab(f, area, &v)).unwrap();
    let text = screen_text(t.backend().buffer());
    assert!(text.contains("1 findings") && text.contains("3 tokens / cost"), "unzoomed is the grid again:\n{text}");
}

#[test]
fn review_focus_cycles_both_ways() {
    use ReviewFocus::*;
    assert!(
        Findings.next() == Summary
            && Summary.next() == Cost
            && Cost.next() == Stage
            && Stage.next() == Findings
    );
    // prev is a real inverse, so it survives four
    for f in [Findings, Summary, Cost, Stage] {
        assert!(f.prev().next() == f, "prev/next must be inverse");
        assert!(f.next() != f.prev(), "four variants: next and prev differ");
    }
}
