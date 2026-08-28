//! Chrome and colour: every box, border, hint and semantic span in one
//! place, so the look can't drift screen to screen.

use crate::review::Severity;
use crate::state::Status;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType,
};
use ratatui::Frame;

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

/// A darker tone of [`ART_WHITE`], for de-emphasised text on the selected run
/// row: the bar itself stays a fixed fill, so anything meant to read as
/// muted against it needs its own fixed, darker grey rather than a
/// terminal-palette colour that might not sit under it correctly.
pub const SELECTED_TEXT: Color = Color::Rgb(0x29, 0x29, 0x29);

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

/// Wall x-positions for `labels`, starting at `start`: n+1 walls for n tabs,
/// each tab's own left wall plus one closing wall at the end. Shared by
/// [`tab_strip_width`] and [`tab_strip`] so they agree on where a tab sits.
fn wall_xs(start: u16, labels: &[Line<'_>]) -> Vec<u16> {
    let mut xs = vec![start];
    for l in labels {
        let prev = *xs.last().unwrap();
        xs.push(prev + 1 + l.width() as u16 + 2);
    }
    xs
}

/// Total width [`tab_strip`] needs for `labels`. Call this first to size the
/// area you hand it.
pub fn tab_strip_width(labels: &[Line<'_>]) -> u16 {
    *wall_xs(0, labels).last().unwrap()
}

/// A row of flush, individually-boxed tabs whose baseline stitches into
/// whatever the caller drew at `area.y + area.height` (normally that box's
/// own top border). Only that one row changes, only in the tab columns.
/// The active tab's walls curl into rounded jambs, opening into the box
/// below; every other wall meets the baseline as a junction. Call this
/// after the content below is drawn, or its border paints over the notch.
///
/// Returns each tab's cell (border + label rows), in `labels` order.
/// [`hit_test`](super::hit_test) shares this geometry, so a click can't
/// target something different from what's drawn. A tab that doesn't fit
/// `area.width` is left undrawn and dropped from the return, like `Buttons`
/// going quiet past its own edge.
pub fn tab_strip(f: &mut Frame, area: Rect, labels: &[Line<'_>], active: usize) -> Vec<Rect> {
    if area.height == 0 || labels.is_empty() {
        return Vec::new();
    }
    let xs = wall_xs(area.x, labels);
    let right = area.x + area.width;
    let n = xs.iter().skip(1).take_while(|&&x| x <= right).count();
    let xs = &xs[..=n];
    // `+ 1`: a cell spans its own left wall through the wall shared with
    // the next cell. Without it, that wall gets drawn twice: once here one
    // column short, again as the next cell's left wall.
    let cells: Vec<Rect> = (0..n)
        .map(|k| Rect { x: xs[k], y: area.y, width: xs[k + 1] - xs[k] + 1, height: area.height.min(2) })
        .collect();
    let border = Style::new().fg(MODAL_BORDER);
    let buf = f.buffer_mut();
    for (k, cell) in cells.iter().enumerate() {
        for x in cell.x..cell.x + cell.width {
            let sym = if x == cell.x && k == 0 {
                "╭"
            } else if x == cell.x + cell.width - 1 && k == n - 1 {
                "╮"
            } else if x == cell.x || x == cell.x + cell.width - 1 {
                "┬"
            } else {
                "─"
            };
            buf[(x, area.y)].set_symbol(sym).set_style(border);
        }
        if area.height > 1 {
            let y = area.y + 1;
            buf[(cell.x, y)].set_symbol("│").set_style(border);
            buf[(cell.x + cell.width - 1, y)].set_symbol("│").set_style(border);
            // Centred: `wall_xs` pads the interior by 2, so one spare column
            // always sits on each side of the label.
            let label_w = labels[k].width() as u16;
            let pad = cell.width.saturating_sub(2).saturating_sub(label_w) / 2;
            buf.set_line(cell.x + 1 + pad, y, &labels[k], label_w);
            // Same flat fill `Tabs::highlight_style` gave the selected
            // label. Patched on top, so the marks/text stay, just recoloured.
            if k == active {
                let text_a =
                    Rect { x: cell.x + 1, y, width: cell.width.saturating_sub(2), height: 1 };
                buf.set_style(text_a, Style::new().bg(Color::White).fg(Color::Black).bold());
            }
        }
    }
    if area.height > 1 {
        let seam_y = area.y + area.height;
        for (w, &x) in xs.iter().enumerate() {
            // Rounded jambs (`╯` `╰`), matching the app's corners elsewhere.
            // Except `w == 0`: that's also the content box's own left
            // corner, nothing to curl away from, so it stays a plain drop.
            let sym = if w == 0 {
                if w == active { "│" } else { "├" }
            } else if w == active {
                "╯"
            } else if w == active + 1 {
                "╰"
            } else {
                "┴"
            };
            buf[(x, seam_y)].set_symbol(sym).set_style(border);
        }
        if let Some(cell) = cells.get(active) {
            for x in cell.x + 1..cell.x + cell.width - 1 {
                buf[(x, seam_y)].set_symbol(" ");
            }
        }
    }
    cells
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

fn gate_chip(label: &str, ok: bool) -> Span<'static> {
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
/// Plain text beside a bold title is where a status goes to hide.
///
/// Four colours, four meanings: grey = still working, cyan = your move,
/// green = landed, yellow = you said no, red = broken.
pub fn status_badge(status: &Status) -> Span<'static> {
    let (label, bg) = match status {
        Status::Planned => ("planned", Color::Gray),
        Status::SpecApproved => ("spec approved", Color::Gray),
        Status::RedOk => ("tests are red", Color::Gray),
        Status::GreenOk => ("tests are green", Color::Gray),
        Status::Reviewed => ("reviewed", Color::Cyan),
        Status::Staged => ("staged", Color::Green),
        Status::Committed => ("committed", Color::Green),
        // A rejection is a decision you made, not a thing that broke.
        Status::Failed(why) => match why.strip_prefix("rejected_") {
            Some(gate) => {
                return Span::styled(
                    format!(" rejected: {gate} "),
                    Style::new().bg(Color::Yellow).fg(Color::Black).bold(),
                )
            }
            // The red already says "failed"; a prefix would only eat the column
            // width the reason itself needs.
            None => (why.as_str(), Color::Red),
        },
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
