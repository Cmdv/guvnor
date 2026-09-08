use guvnor::spec::Spec;
use guvnor::tui::{panel_rows, press, render_spec_panels, screen_text, spec_sections, SpecPanels};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn spec() -> Spec {
    Spec {
        title: "t".into(),
        objective: "make it work".into(),
        files: vec!["src/a.js (new): the thing".into()],
        interfaces: vec!["src/a.js: function f(x) — does x".into()],
        constraints: vec!["no deps".into(), "no globals".into()],
        verification: "node --test".into(),
        acceptance_criteria: vec!["works".into(), "still works".into()],
    }
}

fn screen_of(w: u16, h: u16, p: &SpecPanels, sp: &Spec) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| render_spec_panels(f, Rect::new(0, 0, w, h), sp, p)).unwrap();
    screen_text(t.backend().buffer())
}

fn screen(w: u16, h: u16, p: &SpecPanels) -> String {
    screen_of(w, h, p, &spec())
}

/// Every section keeps its box at every size. Narrow stacks the boxes; it
/// never collapses them to prose — that is the wall of text the boxes exist
/// to break up.
#[test]
fn every_section_keeps_its_box_at_any_size() {
    let p = SpecPanels::default();
    for (w, h) in [(120, 30), (120, 14), (70, 40), (60, 20)] {
        let text = screen(w, h, &p);
        for (n, s) in spec_sections(&spec()).iter().enumerate() {
            // the number is the key that jumps here, so it is part of the title
            let label = format!("{} {}", n + 1, s.title);
            assert!(text.contains(&label), "no box for {label} at {w}x{h}:\n{text}");
        }
    }
    // two columns while there's room, one when there isn't — and the two
    // things the run is judged by keep the full width
    assert_eq!(panel_rows(120), [vec![0, 1], vec![2, 3], vec![4], vec![5]]);
    assert_eq!(panel_rows(70).len(), 6, "narrow stacks them, it does not drop them");
}

/// The objective is what you read first, so it is not allowed to be the
/// smallest box on screen just because it is three sentences and the
/// criteria list is fifteen bullets. The room comes off the criteria, which
/// scroll (6, then ↑↓).
#[test]
fn the_objective_gets_room_and_a_long_criteria_list_does_not_take_it() {
    let mut sp = spec();
    sp.acceptance_criteria = (1..=15).map(|n| format!("criterion number {n}")).collect();
    let (w, h) = (120, 40);
    let text = screen_of(w, h, &SpecPanels::default(), &sp);
    let row = |label: &str| text.lines().position(|l| l.contains(label)).unwrap();
    // rows: objective│files · interfaces│constraints · verification · criteria
    let objective = row("3 Interfaces") - row("1 Objective");
    let criteria = h as usize - row("6 Acceptance criteria");
    assert!(
        objective > criteria,
        "objective {objective} rows vs criteria {criteria} — the box everyone reads lost:\n{text}"
    );
    // ...and the criteria still get more than a sliver: capped, not starved
    assert!(criteria >= 8, "criteria squeezed to {criteria} rows:\n{text}");
}

/// The boxes are how tall the screen allows, so content taller than that has
/// to be reachable: a number gets you to the box, the arrows scroll it.
#[test]
fn a_number_picks_a_box_and_the_arrows_scroll_that_one() {
    let mut p = SpecPanels::default();
    assert!(p.handle(&press(KeyCode::Char('4'))));
    assert_eq!(p.focus, 3);
    // the focused box advertises the arrows; the others don't
    let text = screen(120, 30, &p);
    assert!(text.contains("4 Constraints (2) ↑↓"), "{text}");
    assert!(!text.contains("1 Objective ↑↓"));

    // arrows move that box's offset and no other
    p.scrolls[3].max.set(9);
    assert!(p.handle(&press(KeyCode::Down)));
    assert_eq!(p.scrolls[3].off, 1);
    assert!(p.scrolls.iter().enumerate().all(|(i, s)| i == 3 || s.off == 0));
    // ...and clamped by the same `Scroll` contract as everywhere else
    for _ in 0..50 {
        p.handle(&press(KeyCode::Down));
    }
    assert_eq!(p.scrolls[3].off, 9);
    // keys that aren't ours are handed back: `7` is not a box, `e` is edit
    for c in ['7', '0', 'e'] {
        assert!(!p.handle(&press(KeyCode::Char(c))), "{c} must pass through");
    }
}

/// Zoom is the point of the number, tmux style: pressing it doesn't just move
/// the cursor, it makes that box the only one on the tab. The others — their
/// numbers included — are gone, not just unfocused, until esc puts the grid
/// back (esc itself is the run screen's to handle, not this box's).
#[test]
fn pressing_a_number_zooms_the_box_to_the_whole_tab() {
    let mut p = SpecPanels::default();
    assert!(p.handle(&press(KeyCode::Char('2'))));
    assert!(p.zoomed);
    let text = screen(120, 30, &p);
    assert!(text.contains("2 Files"), "{text}");
    assert!(!text.contains("1 Objective"), "the rest must be gone, not just unfocused:\n{text}");
    assert!(!text.contains("6 Acceptance"), "{text}");
    assert!(text.contains("unzoom"), "the way back belongs on screen: {text}");

    p.zoomed = false;
    let text = screen(120, 30, &p);
    assert!(text.contains("1 Objective") && text.contains("2 Files"), "unzoomed is the grid again:\n{text}");
}

/// Verification is one line (the test command), but a single-content-row box
/// reads as cramped next to its neighbours — it always gets at least two.
#[test]
fn verification_box_is_never_a_single_content_row() {
    let (w, h) = (120, 40);
    let text = screen(w, h, &SpecPanels::default());
    let row = |label: &str| text.lines().position(|l| l.contains(label)).unwrap();
    let verification = row("6 Acceptance criteria") - row("5 Verification");
    assert!(
        verification >= 4,
        "verification box only {verification} rows (want >=4: 2 border + 2 content):\n{text}"
    );
}

/// A box only gets a scrollbar when it actually has more content than room —
/// one that already fits stays clean instead of showing a bar frozen at 100%.
#[test]
fn a_box_only_gets_a_scrollbar_when_its_content_overflows() {
    let mut sp = spec();
    sp.acceptance_criteria = (1..=30).map(|n| format!("criterion number {n}")).collect();
    let mut p = SpecPanels::default();
    p.focus = 5; // Acceptance criteria
    p.zoomed = true;

    let cramped = screen_of(60, 15, &p, &sp);
    assert!(
        cramped.contains('█') || cramped.contains('║'),
        "overflowing box should show a scrollbar:\n{cramped}"
    );

    let roomy = screen_of(60, 40, &p, &sp);
    assert!(
        !roomy.contains('█') && !roomy.contains('║'),
        "a box with nothing to scroll must stay clean:\n{roomy}"
    );
}

/// Tab/backtab are the digits' next/prev: the same six boxes, walked in
/// order instead of jumped to, wrapping at both ends so it is a cycle rather
/// than a dead stop.
#[test]
fn tab_and_backtab_walk_the_boxes_in_order_and_wrap() {
    let mut p = SpecPanels::default();
    assert_eq!(p.focus, 0);
    for want in [1, 2, 3, 4, 5, 0] {
        assert!(p.handle(&press(KeyCode::Tab)));
        assert_eq!(p.focus, want);
    }
    for want in [5, 4, 3, 2, 1, 0] {
        assert!(p.handle(&press(KeyCode::BackTab)));
        assert_eq!(p.focus, want);
    }
}
