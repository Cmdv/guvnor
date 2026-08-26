use guvnor::tui::{
    base64, hang_wrap, hit_test, line_text, osc52_sequence, press, Buttons, LineInput, Scroll,
    TextArea, YES_NO,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[test]
fn hit_test_finds_the_cell_under_a_point_or_none() {
    let cells = [Rect::new(0, 0, 5, 2), Rect::new(5, 0, 5, 2)];
    assert_eq!(hit_test(&cells, Position::new(2, 1)), Some(0));
    assert_eq!(hit_test(&cells, Position::new(7, 0)), Some(1));
    // out of bounds both ways. No hit either way.
    assert_eq!(hit_test(&cells, Position::new(2, 5)), None);
    assert_eq!(hit_test(&cells, Position::new(20, 0)), None);
}

#[test]
fn hang_wrap_keeps_continuations_under_the_marker() {
    // "‣ " marker (width 2) + content long enough to wrap at 12 cols
    let line = Line::from(vec![
        Span::styled("‣ ", Style::new().fg(Color::DarkGray)),
        Span::raw("alpha beta gamma delta epsilon"),
    ]);
    let rows = hang_wrap(&line, 12);
    assert!(rows.len() >= 2, "should wrap: {rows:?}");
    let text = line_text;
    assert!(text(&rows[0]).starts_with("‣ "), "row 0 keeps the marker");
    for r in &rows[1..] {
        let t = text(r);
        assert!(t.starts_with("  "), "continuation not indented: {t:?}");
        assert!(t.chars().count() <= 12, "over width: {t:?}");
    }
    // no word is dropped in the reflow
    let joined: String = rows.iter().map(text).collect();
    for w in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        assert!(joined.contains(w), "lost {w}");
    }
    // a plain one-span line has no marker, so it wraps flush (no indent)
    let plain = hang_wrap(&Line::raw("one two three four five six seven"), 10);
    assert!(plain.len() >= 2);
    assert!(!text(&plain[1]).starts_with(' '), "plain line must not gain an indent");
}

#[test]
fn line_input_edits() {
    let mut i = LineInput::default();
    for c in "abc".chars() {
        i.handle(&KeyEvent::from(KeyCode::Char(c)));
    }
    i.handle(&KeyEvent::from(KeyCode::Left));
    i.handle(&KeyEvent::from(KeyCode::Backspace));
    assert_eq!(i.value, "ac");
    i.handle(&KeyEvent::from(KeyCode::Char('é')));
    assert_eq!(i.value, "aéc");
}

#[test]
fn line_input_respects_max() {
    let mut i = LineInput { max: 2, ..Default::default() };
    for c in "abc".chars() {
        i.handle(&KeyEvent::from(KeyCode::Char(c)));
    }
    assert_eq!(i.value, "ab");
}

/// Ctrl+letter is a shortcut, not text. crossterm reports Ctrl+U as
/// `Char('u') + CONTROL`, so without the guard the reflex for "kill the
/// line" typed a `u` into every field in the app.
#[test]
fn ctrl_letters_are_not_typed_into_a_field() {
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let mut li = LineInput::with("abc");
    for c in ['u', 'a', 'w', 'd', 'v'] {
        li.handle(&ctrl(c));
    }
    assert_eq!(li.value, "abc");
    assert_eq!(li.cursor, 3);

    let mut ta = TextArea::from("abc");
    for c in ['u', 'w'] {
        ta.handle(&ctrl(c));
    }
    assert_eq!(ta.value(), "abc");
    // a plain letter still types
    li.handle(&press(KeyCode::Char('d')));
    assert_eq!(li.value, "abcd");
}

/// A committed run offers no actions, so `Buttons` with no labels is a real
/// state. `len() - 1` on it underflows.
#[test]
fn an_empty_button_row_takes_keys_without_panicking() {
    let mut b = Buttons::new(&[], &[Color::Green, Color::Gray]);
    assert_eq!(b.handle(KeyCode::Right), None);
    assert_eq!(b.handle(KeyCode::Char('l')), None);
    b.next();
    b.prev();
    assert_eq!(b.sel, 0);
}

#[test]
fn line_input_ctrl_arrows_jump_by_word() {
    let mut i = LineInput::with("hello world foo");
    i.cursor = 0;
    i.handle(&KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(i.cursor, 5, "after 'hello'");
    i.handle(&KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(i.cursor, 11, "after 'world'");
    i.handle(&KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(i.cursor, 6, "back to the start of 'world'");
    // plain arrows still move one character, unaffected
    i.handle(&KeyEvent::from(KeyCode::Right));
    assert_eq!(i.cursor, 7);
}

#[test]
fn textarea_newline_and_join() {
    use ratatui::crossterm::event::KeyModifiers;
    let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    let mut t = TextArea::default();
    for c in "ab".chars() {
        t.handle(&KeyEvent::from(KeyCode::Char(c)));
    }
    t.handle(&KeyEvent::from(KeyCode::Left));
    // bare ↵ belongs to the caller — it submits, it does not type
    t.handle(&KeyEvent::from(KeyCode::Enter));
    assert_eq!(t.value(), "ab", "bare ↵ must not reach the text");
    t.handle(&shift_enter);
    assert_eq!(t.value(), "a\nb");
    assert_eq!((t.row, t.col), (1, 0));
    t.handle(&KeyEvent::from(KeyCode::Backspace)); // join back
    assert_eq!(t.value(), "ab");
    assert_eq!((t.row, t.col), (0, 1));
}

/// A long line must continue on the next row, not run off the right edge.
#[test]
fn a_long_line_wraps_and_the_cursor_follows_it() {
    let mut t = TextArea::from("abcdefghij");
    // no spaces to break on: hard at the edge
    let (rows, at) = t.wrapped(4);
    assert_eq!(rows, ["abcd", "efgh", "ij"]);
    assert_eq!(at, (2, 2), "cursor after the 10th char");
    // with a space, the word moves down whole
    let words = TextArea::from("hello world");
    assert_eq!(words.wrapped(8), (vec!["hello ".into(), "world".into()], (1, 5)));
    // an exactly-full row leaves the cursor somewhere to be
    t.col = 8;
    assert_eq!(t.wrapped(4).1, (2, 0));
    t.col = 4;
    assert_eq!(t.wrapped(4).1, (1, 0));
    // real newlines still start a row, and empty ones keep their place
    let t2 = TextArea::from("ab\n\ncd");
    assert_eq!(t2.wrapped(4), (vec!["ab".into(), "".into(), "cd".into()], (2, 2)));
    // nothing typed yet: one row, cursor at the origin
    assert_eq!(TextArea::default().wrapped(10), (vec![String::new()], (0, 0)));
}

#[test]
fn up_down_follow_the_wrapped_row_not_the_logical_line() {
    let mut t = TextArea::from("abcdefgh");
    t.w.set(4); // one logical line, two wrapped rows: "abcd" / "efgh"
    t.col = 6; // second wrapped row, at 'g'
    t.handle(&KeyEvent::from(KeyCode::Up));
    assert_eq!((t.row, t.col), (0, 2), "same column, one wrapped row up");
    t.handle(&KeyEvent::from(KeyCode::Down));
    assert_eq!((t.row, t.col), (0, 6), "back down to where it started");

    // a real second logical line is one more wrapped row past the first
    // line's own wrap, not a jump straight to it
    let mut t2 = TextArea::from("abcdefgh\nZ");
    t2.w.set(4);
    t2.row = 0;
    t2.col = 2; // first wrapped row of line 0
    t2.handle(&KeyEvent::from(KeyCode::Down));
    assert_eq!((t2.row, t2.col), (0, 6), "still line 0, its second wrapped row");
    t2.handle(&KeyEvent::from(KeyCode::Down));
    assert_eq!((t2.row, t2.col), (1, 1), "now line 1, clamped to its length");
}

#[test]
fn shift_arrows_select_and_typing_replaces_it() {
    let mut t = TextArea::from("hello world");
    t.col = 0;
    for _ in 0..5 {
        t.handle(&KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    }
    assert_eq!(t.anchor, Some((0, 0)), "anchor pinned where shift first went down");
    assert_eq!((t.row, t.col), (0, 5));
    t.handle(&KeyEvent::from(KeyCode::Char('X'))); // types over the selection
    assert_eq!(t.value(), "X world");
    assert_eq!(t.anchor, None, "consumed");
    assert_eq!((t.row, t.col), (0, 1));

    // a selection spanning a newline joins across it, same as backspace does
    let mut t2 = TextArea::from("ab\ncd");
    t2.row = 0;
    t2.col = 1;
    t2.handle(&KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)); // selects "b\nc"
    t2.handle(&KeyEvent::from(KeyCode::Backspace));
    assert_eq!(t2.value(), "ad");

    // an unshifted arrow collapses the selection instead of acting on it
    let mut t3 = TextArea::from("hello");
    t3.col = 0;
    t3.handle(&KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    t3.handle(&KeyEvent::from(KeyCode::Right));
    assert_eq!(t3.anchor, None);
    t3.handle(&KeyEvent::from(KeyCode::Backspace));
    assert_eq!(t3.value(), "hllo", "backspace removed one char, not the old selection");
}

#[test]
fn scroll_stops_with_the_last_line_at_the_bottom() {
    let mut s = Scroll::default();
    // 50 wrapped lines in a 10-row box: the last reachable offset is 40,
    // which puts line 50 on the bottom row — one more would show blank space
    assert_eq!(s.fit(50, 10), 0);
    for _ in 0..100 {
        s.by(1);
    }
    assert_eq!(s.off, 40, "scrolled past the end of the content");
    assert_eq!(s.fit(50, 10), 40);
    s.by(-1000);
    assert_eq!(s.off, 0, "can't scroll above the first line either");
    // content that already fits never moves at all
    let mut short = Scroll::default();
    short.fit(3, 10);
    short.by(5);
    assert_eq!(short.off, 0);
    // a resize shrinking the content pulls a stale offset back into range
    // rather than drawing an empty box for a frame
    s.by(1000);
    assert_eq!(s.fit(12, 10), 2);
}

#[test]
fn armed_button_is_filled_edge_to_edge_and_both_keep_their_outline() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let b = Buttons::new(&["continue", "skip"], YES_NO); // sel = 0
    let mut t = Terminal::new(TestBackend::new(40, 3)).unwrap();
    t.draw(|f| b.render(f, Rect::new(0, 0, 40, 3), true)).unwrap();
    let buf = t.backend().buffer().clone();

    // width is label + 6, so `continue` occupies x 0..14 and `skip` 16..26
    let inner_y = 1;
    let filled: Vec<_> =
        (1..13).map(|x| buf[(x, inner_y)].style().bg).collect();
    assert!(
        filled.iter().all(|bg| *bg == Some(Color::Green)),
        "armed button must be filled across its whole inner width, got {filled:?}"
    );
    // the unarmed button is outlined only — no fill bleeding across
    let unarmed = buf[(18, inner_y)].style().bg;
    assert!(
        unarmed != Some(Color::Red) && unarmed != Some(Color::Green),
        "skip should not be filled, got {unarmed:?}"
    );
    // both outlines carry their own colour even though only one is armed
    assert_eq!(buf[(0, 0)].style().fg, Some(Color::Green), "continue outline");
    assert_eq!(buf[(16, 0)].style().fg, Some(Color::Red), "skip outline");

    // unfocused: still outlined in their colours, nothing filled
    let mut t2 = Terminal::new(TestBackend::new(40, 3)).unwrap();
    t2.draw(|f| b.render(f, Rect::new(0, 0, 40, 3), false)).unwrap();
    let buf2 = t2.backend().buffer().clone();
    assert_eq!(buf2[(0, 0)].style().fg, Some(Color::Green));
    assert_eq!(buf2[(16, 0)].style().fg, Some(Color::Red));
    assert!(
        (1..13).all(|x| buf2[(x, inner_y)].style().bg != Some(Color::Green)),
        "nothing is armed when the section is unfocused"
    );
}

#[test]
fn buttons_row_is_the_only_way_to_act() {
    let mut b = Buttons::new(&["continue", "skip"], YES_NO);
    // index 0 preselected: the common case is "↓ onto the row, ↵"
    assert_eq!(b.handle(KeyCode::Enter), Some(0));
    // ←/→ move and clamp — no wrap, so you can't overshoot onto `skip`
    assert_eq!(b.handle(KeyCode::Left), None);
    assert_eq!(b.handle(KeyCode::Enter), Some(0));
    b.handle(KeyCode::Right);
    assert_eq!(b.handle(KeyCode::Enter), Some(1));
    b.handle(KeyCode::Right);
    assert_eq!(b.handle(KeyCode::Enter), Some(1), "clamped at the last button");
    // no letter shortcuts: the row is reached and fired with the keys the
    // hint bar already shows, not with one guessed from a label
    let mut c = Buttons::new(&["continue", "skip"], YES_NO);
    for k in [KeyCode::Char('c'), KeyCode::Char('s'), KeyCode::Char(' '), KeyCode::Tab] {
        assert_eq!(c.handle(k), None, "{k:?} must not act");
    }
    assert_eq!(c.sel, 0, "and none of them moved the cursor either");
    // destructive rows put the safe answer at 0
    let mut d = Buttons::new(&["cancel", "delete"], YES_NO);
    assert_eq!(d.handle(KeyCode::Enter), Some(0));
}

#[test]
fn base64_matches_known_vectors() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
}

#[test]
fn osc52_wraps_for_tmux_and_doubles_its_own_escapes() {
    let plain = osc52_sequence("hi", false);
    assert!(plain.starts_with("\x1b]52;c;") && plain.ends_with('\u{7}'), "{plain:?}");
    let wrapped = osc52_sequence("hi", true);
    assert!(wrapped.starts_with("\x1bPtmux;\x1b\x1b]52;c;"), "{wrapped:?}");
    assert!(wrapped.ends_with("\x1b\\"), "{wrapped:?}");
}
