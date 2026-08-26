use guvnor::state::Status;
use guvnor::tui::{
    art_lines, boxed, box_title, gates_line, hint_line, pad_badge, status_badge, tab_strip,
    tab_strip_width, ART_SHADE, ART_WHITE, MODAL_BORDER,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Block;

#[test]
fn a_box_title_reds_the_letter_that_opens_it() {
    // one device, everywhere: red glyph means "press this". The review
    // sections advertise f / r / t and the letters must match the titles.
    for (key, title) in [("f", "findings — 0 of 2"), ("r", "reviewer comment"), ("t", "tokens / cost")] {
        let l = box_title(title, key, Style::new(), false);
        let red: Vec<&str> = l
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Red))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(red, [key], "{title} must show exactly {key} in red");
        // the whole title survives being cut in three
        let all: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all.contains(title), "{all:?} lost the title");
    }
    // no key, no red: a box you can only tab to must not promise a letter
    let plain = box_title("6 file(s) this will change", "", Style::new(), false);
    assert!(plain.spans.iter().all(|s| s.style.fg != Some(Color::Red)));
}

#[test]
fn hint_line_embeds_red_letter() {
    let line = hint_line(&[("f", "filter"), ("esc", "back")]);
    // "f" embedded, label grey, brackets style-less so they inherit the box:
    // ["─┘ ", "", red "f", "ilter", " └", "─┘ ", "esc", " back", " └"]
    assert!(line.spans.iter().any(|s| s.content == "f" && s.style.fg == Some(Color::Red)));
    assert!(line.spans.iter().any(|s| s.content == "ilter" && s.style.fg == Some(MODAL_BORDER)));
    assert!(line.spans.iter().any(|s| s.content == "esc"));
    // the hanging brackets carry no colour of their own — they take the
    // box's title_style, which is how a focused box lights them up too
    assert!(line.spans.iter().any(|s| s.content == " └" && s.style.fg.is_none()));
}

/// The invariant focus styling rests on: a box's hanging brackets take its
/// `title_style`, so lighting the border white lights the brackets with it.
#[test]
fn title_style_colours_the_hanging_brackets() {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    // "┐" is unique to the top title bracket, "┘" to the bottom hint's —
    // both must take the box's title_style, top and bottom alike.
    let bracket_fg = |block: Block<'static>, glyph: &str| -> Option<Color> {
        let mut t = Terminal::new(TestBackend::new(20, 3)).unwrap();
        t.draw(|f| f.render_widget(Paragraph::new("").block(block), Rect::new(0, 0, 20, 3)))
            .unwrap();
        let buf = t.backend().buffer().clone();
        (0..3)
            .flat_map(|y| (0..20).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].clone())
            .find(|c| c.symbol() == glyph)
            .and_then(|c| c.style().fg)
    };
    let plain = || boxed("hi", Style::new().bold()).title_bottom(hint_line(&[("q", "quit")]));
    let lit = || plain().title_style(Style::new().fg(Color::White));
    // a plain box: grey brackets top and bottom, matching the grey border
    assert_eq!(bracket_fg(plain(), "┐"), Some(MODAL_BORDER), "top bracket grey");
    assert_eq!(bracket_fg(plain(), "┘"), Some(MODAL_BORDER), "bottom bracket grey");
    // a focus-lit box: both light white with the border
    assert_eq!(bracket_fg(lit(), "┐"), Some(Color::White), "top bracket white");
    assert_eq!(bracket_fg(lit(), "┘"), Some(Color::White), "bottom bracket white");
}

#[test]
fn art_lines_shade_runs_solid_rest_filled() {
    let lines = art_lines("░░█▀░", Style::new().fg(ART_WHITE));
    assert_eq!(lines.len(), 1);
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].content, "░░");
    assert_eq!(spans[0].style.bg, Some(ART_SHADE)); // solid: fg==bg
    assert_eq!(spans[1].content, "█▀");
    assert_eq!(spans[1].style.fg, Some(ART_WHITE));
    assert_eq!(spans[2].content, "░");
}

#[test]
fn pad_badge_widens_the_block_and_keeps_its_colour() {
    let reviewed = status_badge(&Status::Reviewed); // " reviewed ", cyan
    let w = status_badge(&Status::SpecApproved).content.chars().count(); // " spec approved "
    let padded = pad_badge(reviewed.clone(), w);
    assert_eq!(padded.content.chars().count(), w, "padded out to the widest");
    assert_eq!(padded.style, reviewed.style, "colour untouched");
    assert!(padded.content.starts_with(" reviewed "), "label stays, block grows right");
    // an already-wide chip is left exactly as it is
    assert_eq!(pad_badge(reviewed.clone(), 3).content, reviewed.content);
}

#[test]
fn gates_line_chips() {
    let mut g = guvnor::state::Gates::default();
    g.spec.approved = true;
    let line = gates_line(&g);
    assert_eq!(line.width(), 27); // 8+9+8 chips + 2 dividers — runs column relies on it
    assert_eq!(line.spans[0].content, " Spec ✓ ");
    assert_eq!(line.spans[0].style.bg, Some(Color::Green));
    assert_eq!(line.spans[2].content, " Tests · ");
    assert_eq!(line.spans[2].style.bg, None);
}

/// A status nobody can pick out of a title line is not a status. Every one
/// of them is filled, and the colour is the class, not the string.
#[test]
fn every_status_is_a_filled_chip_in_words() {
    let failed = |w: &str| Status::Failed(w.to_string());
    for s in [
        Status::Planned,
        Status::SpecApproved,
        Status::RedOk,
        Status::GreenOk,
        Status::Reviewed,
        Status::Staged,
        Status::Committed,
        failed("vacuous_tests"),
        failed("rejected_work"),
    ] {
        let b = status_badge(&s);
        assert!(b.style.bg.is_some(), "{s} must be filled, not plain text");
        assert!(!b.content.contains('_') || matches!(s, Status::Failed(_)),
                "{s} rendered as the machine string: {}", b.content);
    }
    // the classes: landed is green, your-move is cyan, broken is red, and a
    // rejection is a decision (yellow) rather than a fault
    assert_eq!(status_badge(&Status::Committed).style.bg, Some(Color::Green));
    assert_eq!(status_badge(&Status::Staged).style.bg, Some(Color::Green));
    assert_eq!(status_badge(&Status::Reviewed).style.bg, Some(Color::Cyan));
    assert_eq!(status_badge(&Status::Planned).style.bg, Some(Color::Gray));
    assert_eq!(status_badge(&failed("vacuous_tests")).style.bg, Some(Color::Red));
    // the prefix is dead weight next to a red chip; the reason is not
    assert_eq!(status_badge(&failed("vacuous_tests")).content, " vacuous_tests ");
    let r = status_badge(&failed("rejected_work"));
    assert_eq!(r.style.bg, Some(Color::Yellow));
    assert_eq!(r.content, " rejected: work ");
}

#[test]
fn tab_strip_width_matches_the_wall_math_it_shares_with_tab_strip() {
    // one tab: its own left wall (1) + " Spec " (6) = 7
    assert_eq!(tab_strip_width(&[Line::raw("Spec")]), 7);
    // two tabs share the middle wall, so it's not counted twice
    assert_eq!(tab_strip_width(&[Line::raw("Spec"), Line::raw("Tests")]), 7 + 1 + 7);
}

/// Opens under the active tab (rounded jambs, blank interior), a
/// junction under every inactive one, and a plain drop where a wall
/// lands on the content box's own left corner.
#[test]
fn tab_strip_opens_under_the_active_tab_and_returns_click_geometry() {
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    let labels = vec![Line::raw("Spec"), Line::raw("Tests"), Line::raw("Work")];
    let mut cells = Vec::new();
    let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
    t.draw(|f| {
        // drawn first: the strip's seam must overwrite this border, not
        // the reverse.
        f.render_widget(
            Paragraph::new("").block(boxed("body", Style::new())),
            Rect::new(0, 2, 40, 4),
        );
        cells = tab_strip(f, Rect::new(0, 0, 40, 2), &labels, 1); // "Tests" active
    })
    .unwrap();
    assert_eq!(cells.len(), 3, "one cell per label");
    assert_eq!((cells[0].y, cells[0].height), (0, 2), "border + label rows");
    assert_eq!(cells[1].x, 7, "Tests starts right after Spec's wall");

    let buf = t.backend().buffer().clone();
    // by column, not byte: box-drawing glyphs are multi-byte and a
    // sliced String would cut one in half.
    let sym = |x: u16, y: u16| buf[(x, y)].symbol().to_string();
    let row: Vec<String> = (0..40).map(|x| sym(x, 2)).collect();
    assert_eq!(sym(0, 2), "├", "left edge joins the box below, not a plain corner: {row:?}");
    assert_eq!(sym(7, 2), "╯", "Tests' own left wall curls into a jamb: {row:?}");
    assert_eq!(sym(9, 2), " ", "and its interior is an open doorway: {row:?}");
    assert_eq!(sym(15, 2), "╰", "Tests' own right wall curls into a jamb: {row:?}");
    assert_eq!(sym(22, 2), "┴", "an inactive wall meets the baseline as a junction: {row:?}");
    // a shared wall is one column, not two: Spec's right wall and
    // Tests' left wall must both land on x=7, no stray second one at 8.
    assert_eq!(sym(7, 0), "┬", "one junction, not a pair either side of it");
    assert_eq!(sym(8, 0), "─", "no second junction beside it");
    assert_eq!(sym(7, 1), "│", "one wall, not a pair either side of it");
    assert_eq!(sym(9, 1), "T", "Tests' label starts right after its own pad");
    // the active tab gets the same flat highlight `Tabs` gave the
    // selected label, patched onto the label, not the border.
    assert_eq!(buf[(9, 1)].style().bg, Some(Color::White), "active tab is highlighted");
    assert_eq!(buf[(9, 1)].style().fg, Some(Color::Black));
    assert_ne!(buf[(2, 1)].style().bg, Some(Color::White), "an inactive tab keeps its own colour");
}

/// The exception the rule above carves out: an active tab's left wall,
/// when it's also the group's leftmost, has nothing to curl away from.
#[test]
fn the_first_tab_stays_a_plain_drop_when_active() {
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    let labels = vec![Line::raw("Spec"), Line::raw("Tests")];
    let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
    t.draw(|f| {
        f.render_widget(
            Paragraph::new("").block(boxed("body", Style::new())),
            Rect::new(0, 2, 40, 4),
        );
        tab_strip(f, Rect::new(0, 0, 40, 2), &labels, 0); // "Spec" active
    })
    .unwrap();
    let buf = t.backend().buffer().clone();
    assert_eq!(buf[(0, 2)].symbol(), "│", "no jamb to curl into on the left edge");
}
