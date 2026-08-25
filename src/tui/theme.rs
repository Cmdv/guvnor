//! Chrome and colour: every box, border, hint and semantic span in one
//! place, so the look can't drift screen to screen.

use crate::review::Severity;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType,
};

pub const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub const GUV_LETTER: &str = "\
░█▀▀░█░█░█░█░▀░█▀█░█▀█░█▀▄
░█░█░█░█░▀▄▀░░░█░█░█░█░█▀▄
░▀▀▀░▀▀▀░░▀░░░░▀░▀░▀▀▀░▀░▀";

pub const GUV_MASK: &str = r"
            ░░░░░░░░
         ░░░░██████░░░░
       ░░░████████████░░░░
    ░░░███████████████████░░
   ░████████████████████████░░
 ░░██████████████████████████░░
 ░░░░░░░░░░██████████░░░░░░░░░░
 ░█████████░░░████░░░█████████░
░░█░░░░░░░░░██░██░██░░░░░░░░░█░░
░█░░░░░░░░░░░░░░░░░░░░░░░░░░░░█░
░░█░░░░░░░░░░░░██░░░░░░░░░░░░█░░
 ░██░░░░░░░░░░░██░░░░░░░░░░░██░
 ░███████░█░████████░█░███████░
 ░████████████░██░████████████░
 ░░███████████░██░███████████░░
  ░░░████████░████░███████░░░
     ░░███░██░░░░░░██░███░░░
      ░░██░█░░████░░█░██░░
       ░███░        ░███░
       ░░██░░      ░░██░░
       ░░██░░      ░░██░░
        ░██░░      ░░██░
        ░░█░░      ░░█░░
        ░░█░░      ░░█░░
         ░░░░      ░░░░";

// logo palette (sampled from the reference artwork): one shade tone shared by
// lettering patches and mask halo — exact, independent of terminal font/theme.
pub const ART_SHADE: Color = Color::Rgb(0x3a, 0x41, 0x50);

pub const ART_WHITE: Color = Color::Rgb(0xea, 0xea, 0xea);

/// Every modal wears the same chrome: one dark grey fill, one border colour.
/// Colour inside a modal means something (button accent, severity, danger) —
/// it is never used to tell one modal apart from another.
pub const MODAL_BG: Color = Color::Rgb(0x1e, 0x21, 0x28);

pub const MODAL_BORDER: Color = Color::Rgb(0x8a, 0x93, 0xa8);

/// Every box in the app: rounded corners, and the title hung off the top border
/// in its own brackets rather than sitting on it. One entry point, so the chrome
/// can't drift box to box.
pub fn boxed(title: &str, style: Style) -> Block<'static> {
    // `title_style` colours the hanging brackets on every title (top and
    // bottom): grey by default, so it matches the grey border. A focus-lit box
    // overrides both border_style and title_style to white together.
    let b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MODAL_BORDER))
        .title_style(Style::new().fg(MODAL_BORDER));
    if title.is_empty() {
        b
    } else {
        b.title(box_title(title, "", style, false))
    }
}

/// `─┐ title ┌` (top) / `─┘ title └` (bottom): the brackets turn away from the
/// line, so the label reads as hanging off the border, not breaking it.
pub fn box_title(title: &str, key: &str, style: Style, bottom: bool) -> Line<'static> {
    let (l, r) = if bottom { ("─┘", "└") } else { ("─┐", "┌") };
    // The brackets carry no colour of their own: they inherit the block's
    // `title_style`, which every box sets to match its border — so a focused
    // box lights its hanging brackets white along with the line they hang off.
    // A plain title takes the calm grey; an explicit colour (danger red, landed
    // green) is left alone, and the red key always means "press this".
    let style = if style.fg.is_none() { style.fg(MODAL_BORDER) } else { style };
    let mut spans = vec![Span::raw(l), Span::raw(" ")];
    match (key.len() == 1).then(|| title.find(key)).flatten() {
        Some(i) => {
            spans.push(Span::styled(title[..i].to_string(), style));
            spans.push(Span::styled(key.to_string(), Style::new().fg(Color::Red).bold()));
            spans.push(Span::styled(title[i + 1..].to_string(), style));
        }
        None => spans.push(Span::styled(title.to_string(), style)),
    }
    spans.push(Span::raw(" "));
    spans.push(Span::raw(r));
    Line::from(spans)
}

pub fn modal(title: &str, hints: &[(&str, &str)]) -> Block<'static> {
    boxed(title, Style::new().bold())
        .border_style(Style::new().fg(MODAL_BORDER))
        .style(Style::new().bg(MODAL_BG))
        .title_bottom(hint_line(hints))
}

/// A section inside a modal: same border colour as its parent, distinguished by
/// its title. `focused` shows whether the arrows act on it — a pane holding more
/// than it can display needs to say when it's the one being scrolled. `key` is
/// the letter that jumps straight here, or "" for a box you can only tab to.
pub fn focus_box(key: &str, title: &str, focused: bool) -> Block<'static> {
    let (border, text) = if focused {
        (Color::White, Style::new().fg(Color::White).bold())
    } else {
        (MODAL_BORDER, Style::new().fg(MODAL_BORDER))
    };
    let mark = if focused { "↑↓ " } else { "" };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title_style(Style::new().fg(border))
        .title(box_title(&format!("{mark}{title}"), key, text, false))
}

/// Colour art per cell class, art string untouched: `░` cells render as solid
/// shade (fg==bg), every other glyph gets `fill`.
pub fn art_lines(art: &str, fill: Style) -> Vec<Line<'static>> {
    let shade = Style::new().fg(ART_SHADE).bg(ART_SHADE);
    art.lines()
        .map(|l| {
            let mut spans: Vec<Span> = Vec::new();
            let mut cur = String::new();
            let mut cur_shade = false;
            for c in l.chars() {
                let s = c == '░';
                if s != cur_shade && !cur.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut cur), if cur_shade { shade } else { fill }));
                }
                cur_shade = s;
                cur.push(c);
            }
            if !cur.is_empty() {
                spans.push(Span::styled(cur, if cur_shade { shade } else { fill }));
            }
            Line::from(spans)
        })
        .collect()
}

// ---- small pure helpers ----------------------------------------------------

pub fn spin_frame() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    SPIN[(ms / 100) as usize % SPIN.len()]
}

pub fn gate_chip(label: &str, ok: bool) -> Span<'static> {
    if ok {
        Span::styled(format!(" {label} ✓ "), Style::new().bg(Color::Green).fg(Color::Black).bold())
    } else {
        Span::styled(format!(" {label} · "), Style::new().fg(Color::DarkGray))
    }
}

/// Boxed gate chips with dividers: ` Spec ✓ │ Tests ✓ │ Work · ` — green bg
/// when approved.
pub fn gates_line(g: &crate::state::Gates) -> Line<'static> {
    let div = || Span::styled("│", Style::new().fg(Color::DarkGray));
    Line::from(vec![
        gate_chip("Spec", g.spec.approved),
        div(),
        gate_chip("Tests", g.tests.approved),
        div(),
        gate_chip("Work", g.work.approved),
    ])
}

/// The run's state as a filled chip, in words instead of the machine string.
/// Plain text beside a bold title is where a status goes to hide, and the old
/// version only coloured `merged` — a string Phase 2j retired, so every landed
/// run rendered as undifferentiated white.
///
/// Four colours, four meanings: grey = still working, cyan = your move,
/// green = landed, yellow = you said no, red = broken.
pub fn status_badge(status: &str) -> Span<'static> {
    let (label, bg) = match status {
        "planned" => ("planned", Color::Gray),
        "spec_approved" => ("spec approved", Color::Gray),
        "red_ok" => ("tests are red", Color::Gray),
        "green_ok" => ("tests are green", Color::Gray),
        "reviewed" => ("reviewed", Color::Cyan),
        "staged" => ("staged", Color::Green),
        "committed" => ("committed", Color::Green),
        // A rejection is a decision you made, not a thing that broke.
        s if s.starts_with("failed:rejected_") => {
            return Span::styled(
                format!(" rejected: {} ", &s["failed:rejected_".len()..]),
                Style::new().bg(Color::Yellow).fg(Color::Black).bold(),
            )
        }
        // The red already says "failed"; the prefix only eats column width that
        // the reason itself needs.
        s => (s.strip_prefix("failed:").unwrap_or(s), Color::Red),
    };
    Span::styled(format!(" {label} "), Style::new().bg(bg).fg(Color::Black).bold())
}

/// Right-pad a status chip to `width` cells (keeping its colour) so a column of
/// them lines up — the coloured block extends, the label stays put.
pub fn pad_badge(badge: Span<'static>, width: usize) -> Span<'static> {
    let len = badge.content.chars().count();
    if len >= width {
        return badge;
    }
    Span::styled(format!("{}{}", badge.content, " ".repeat(width - len)), badge.style)
}

pub fn verdict_span(verdict: &str) -> Span<'static> {
    let style = match verdict {
        "APPROVED" => Style::new().fg(Color::Green),
        "WARNING" => Style::new().fg(Color::Yellow),
        "BLOCKED" => Style::new().fg(Color::Red),
        _ => Style::new(),
    };
    Span::styled(verdict.to_string(), style)
}

/// Section divider used inside tab bodies and the findings list.
pub fn rule(label: &str, color: Color) -> Line<'static> {
    Line::styled(format!("── {label} ──"), Style::new().fg(color).bold())
}

pub fn severity_style(s: Severity) -> Style {
    Style::new().fg(match s {
        Severity::High => Color::Red,
        Severity::Medium => Color::Yellow,
        _ => Color::DarkGray,
    })
}

/// Key hints for a bottom border, in the same hung-label brackets as titles —
/// `─┘ quit └`, with the key picked out in red inside its own word where it fits.
pub fn hint_line(pairs: &[(&str, &str)]) -> Line<'static> {
    let red = Style::new().fg(Color::Red).bold();
    let grey = Style::new().fg(MODAL_BORDER);
    let mut spans = Vec::new();
    for (k, label) in pairs {
        spans.push(Span::raw("─┘ ")); // bracket: inherits the box's title_style
        match (k.len() == 1).then(|| label.find(*k)).flatten() {
            Some(pos) => {
                spans.push(Span::styled(label[..pos].to_string(), grey));
                spans.push(Span::styled(k.to_string(), red));
                spans.push(Span::styled(label[pos + 1..].to_string(), grey));
            }
            None => {
                spans.push(Span::styled(k.to_string(), red));
                spans.push(Span::styled(format!(" {label}"), grey));
            }
        }
        spans.push(Span::raw(" └")); // bracket: inherits the box's title_style
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The proof the focus fix rests on: a box's hanging brackets take its
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
        let reviewed = status_badge("reviewed"); // " reviewed ", cyan
        let w = status_badge("spec_approved").content.chars().count(); // " spec approved "
        let padded = pad_badge(reviewed.clone(), w);
        assert_eq!(padded.content.chars().count(), w, "padded out to the widest");
        assert_eq!(padded.style, reviewed.style, "colour untouched");
        assert!(padded.content.starts_with(" reviewed "), "label stays, block grows right");
        // an already-wide chip is left exactly as it is
        assert_eq!(pad_badge(reviewed.clone(), 3).content, reviewed.content);
    }

    #[test]
    fn gates_line_chips() {
        let mut g = crate::state::Gates::default();
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
        for s in ["planned", "spec_approved", "red_ok", "green_ok", "reviewed", "staged",
                  "committed", "failed:vacuous_tests", "failed:rejected_work"] {
            let b = status_badge(s);
            assert!(b.style.bg.is_some(), "{s} must be filled, not plain text");
            assert!(!b.content.contains('_') || s.starts_with("failed:"),
                    "{s} rendered as the machine string: {}", b.content);
        }
        // the classes: landed is green, your-move is cyan, broken is red, and a
        // rejection is a decision (yellow) rather than a fault
        assert_eq!(status_badge("committed").style.bg, Some(Color::Green));
        assert_eq!(status_badge("staged").style.bg, Some(Color::Green));
        assert_eq!(status_badge("reviewed").style.bg, Some(Color::Cyan));
        assert_eq!(status_badge("planned").style.bg, Some(Color::Gray));
        assert_eq!(status_badge("failed:vacuous_tests").style.bg, Some(Color::Red));
        // the prefix is dead weight next to a red chip; the reason is not
        assert_eq!(status_badge("failed:vacuous_tests").content, " vacuous_tests ");
        let r = status_badge("failed:rejected_work");
        assert_eq!(r.style.bg, Some(Color::Yellow));
        assert_eq!(r.content, " rejected: work ");
    }

}
