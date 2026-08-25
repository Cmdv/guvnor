//! The four hand-rolled controls the whole TUI is built from: a line of
//! text, a block of text, a row of buttons, and a scroll offset that knows where
//! its content ends.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::*;

/// A scroll offset that cannot run past its content: the last line stops at the
/// bottom of the box instead of drifting off into blank space. `render` knows the
/// wrapped height and the viewport, the key handler doesn't — so render records
/// the ceiling (`Cell`, because rendering only has `&self`) and keys clamp to it.
#[derive(Default)]
pub struct Scroll {
    pub off: u16,
    pub max: std::cell::Cell<u16>,
}

impl Scroll {
    /// Move by `d` lines, clamped to what the last frame said was reachable.
    pub fn by(&mut self, d: i32) {
        self.off = ((self.off as i32 + d).max(0) as u16).min(self.max.get());
    }

    pub fn top(&mut self) {
        self.off = 0;
    }

    /// Record the ceiling for `content` wrapped lines in an `h`-row viewport and
    /// return the offset to draw at — clamped, so a resize can't leave a stale
    /// offset showing an empty box for one frame.
    pub fn fit(&self, content: usize, h: u16) -> u16 {
        self.max.set((content as u16).saturating_sub(h));
        self.off.min(self.max.get())
    }
}

// ---- tiny line input (ponytail: chars+backspace+arrows; no word jumps) ----

#[derive(Default)]
pub struct LineInput {
    pub value: String,
    pub cursor: usize, // char index
    pub max: usize,    // 0 = unlimited
}

impl LineInput {
    pub fn with(value: &str) -> Self {
        Self { value: value.to_string(), cursor: char_count(value), max: 0 }
    }

    pub fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor)
            .unwrap_or(self.value.len())
    }

    pub fn handle(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Char(c) => {
                if self.max > 0 && self.value.chars().count() >= self.max {
                    return;
                }
                let i = self.byte_index();
                self.value.insert(i, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let i = self.byte_index();
                self.value.remove(i);
            }
            KeyCode::Left if self.cursor > 0 => self.cursor -= 1,
            KeyCode::Right if self.cursor < self.value.chars().count() => self.cursor += 1,
            _ => {}
        }
    }
}

// ---- tiny multiline input (ponytail: no wrap-aware cursor, no selections) --

pub struct TextArea {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize, // char index
}

impl Default for TextArea {
    fn default() -> Self {
        Self { lines: vec![String::new()], row: 0, col: 0 }
    }
}

pub fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// What turns ↵ into a newline. Shift is the one people reach for; alt is here
/// because a terminal without the kitty keyboard protocol cannot report
/// shift+enter at all, and option/alt+enter it usually can. `run()` asks for the
/// protocol, so on a modern terminal ⇧↵ is what you'll use.
pub fn newline_mods() -> ratatui::crossterm::event::KeyModifiers {
    use ratatui::crossterm::event::KeyModifiers as M;
    M::SHIFT | M::ALT
}

/// Break a line to width `w`: rows of `(first char offset, text)`, split on the
/// last space that fits and hard at the edge when there isn't one. The single
/// definition, so text drawn from it and a cursor computed from it agree.
pub fn wrap_line(line: &str, w: usize) -> Vec<(usize, String)> {
    let w = w.max(1);
    let chars: Vec<char> = line.chars().collect();
    let mut rows = Vec::new();
    let mut i = 0;
    while i + w < chars.len() {
        let cut = chars[i..i + w]
            .iter()
            .rposition(|c| *c == ' ')
            .map(|p| i + p + 1)
            .unwrap_or(i + w);
        rows.push((i, chars[i..cut].iter().collect()));
        i = cut;
    }
    rows.push((i, chars[i..].iter().collect())); // the tail, and empty lines
    rows
}

/// Word-wrap a styled line to width `w` with a hanging indent: continuation rows
/// line up under the text *after* the leading marker (`• `, `12. `) instead of
/// falling back to the margin. The indent is the width of the line's first span
/// when it has a marker plus content (two or more spans); a plain one-span line
/// wraps flush. Span styles are carried across the break.
///
/// `Paragraph`'s own `Wrap` restarts every continuation at column 0, so bullet
/// lists rendered through it lose their shape the moment a line wraps — this is
/// the single place that fixes that, for any box that renders bullets.
pub fn hang_wrap(line: &Line<'static>, w: usize) -> Vec<Line<'static>> {
    let w = w.max(1);
    let flat: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| {
            let st = s.style;
            s.content.chars().map(move |c| (c, st))
        })
        .collect();
    // Fits (or empty): one row, spans untouched.
    if flat.len() <= w {
        return vec![line.clone()];
    }
    let indent = if line.spans.len() >= 2 {
        line.spans[0].content.chars().count().min(w - 1)
    } else {
        0
    };
    // Break points over the flat char stream: full width on row 0, width minus
    // the indent after, splitting on the last space that fits (hard at the edge
    // when a single word is wider than the row).
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < flat.len() {
        let avail = if cuts.is_empty() { w } else { w - indent };
        if i + avail >= flat.len() {
            cuts.push((i, flat.len()));
            break;
        }
        let cut = flat[i..i + avail]
            .iter()
            .rposition(|(c, _)| *c == ' ')
            .map(|p| i + p + 1)
            .unwrap_or(i + avail);
        cuts.push((i, cut));
        i = cut;
    }
    let pad = Style::new().fg(Color::DarkGray);
    cuts.iter()
        .enumerate()
        .map(|(row, &(a, b))| {
            let mut spans: Vec<Span> = Vec::new();
            if row > 0 && indent > 0 {
                spans.push(Span::styled(" ".repeat(indent), pad));
            }
            // Re-coalesce the char run into spans, one per style change.
            let mut buf = String::new();
            let mut cur: Option<Style> = None;
            for &(c, st) in &flat[a..b] {
                if cur == Some(st) {
                    buf.push(c);
                } else {
                    if let Some(cs) = cur {
                        spans.push(Span::styled(std::mem::take(&mut buf), cs));
                    }
                    buf.push(c);
                    cur = Some(st);
                }
            }
            if let Some(cs) = cur {
                spans.push(Span::styled(buf, cs));
            }
            Line::from(spans)
        })
        .collect()
}

/// `hang_wrap` over a whole list, flattened — a section body wrapped for its box.
pub fn hang_wrap_all(lines: &[Line<'static>], w: usize) -> Vec<Line<'static>> {
    lines.iter().flat_map(|l| hang_wrap(l, w)).collect()
}

/// Draw a `TextArea` soft-wrapped in `area`, keeping the cursor row in view.
/// Every multiline input wants exactly this; none of them should own the
/// arithmetic.
pub fn render_textarea(f: &mut Frame, area: Rect, t: &TextArea, focused: bool) {
    let (rows, (cr, cc)) = t.wrapped(area.width as usize);
    let off = cr.saturating_sub(area.height.saturating_sub(1) as usize);
    f.render_widget(Paragraph::new(rows.join("\n")).scroll((off as u16, 0)), area);
    if focused {
        f.set_cursor_position(ratatui::layout::Position::new(
            area.x + cc as u16,
            area.y + (cr - off) as u16,
        ));
    }
}

impl TextArea {
    /// Seed it with existing text, cursor at the end — a draft you can edit,
    /// not a wall you have to retype.
    pub fn from(text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let lines = if lines.is_empty() { vec![String::new()] } else { lines };
        let row = lines.len() - 1;
        let col = char_count(&lines[row]);
        Self { lines, row, col }
    }

    pub fn byte_index(line: &str, col: usize) -> usize {
        line.char_indices().map(|(i, _)| i).nth(col).unwrap_or(line.len())
    }

    pub fn value(&self) -> String {
        self.lines.join("\n")
    }

    /// Display rows for width `w`, and where the cursor sits among them.
    ///
    /// The wrapping is ours rather than `Paragraph`'s because the cursor has to
    /// land exactly on its glyph, and only whoever chose the break points knows
    /// where that is.
    pub fn wrapped(&self, w: usize) -> (Vec<String>, (usize, usize)) {
        let w = w.max(1);
        let mut out: Vec<String> = Vec::new();
        let mut at = (0, 0);
        for (r, line) in self.lines.iter().enumerate() {
            let start = out.len();
            let rows = wrap_line(line, w);
            // one past a full row: the cursor belongs on the next one, which
            // nothing has put there yet
            let mut spill = false;
            if r == self.row {
                let k = rows.iter().rposition(|(off, _)| *off <= self.col).unwrap_or(0);
                let (row, col) = (start + k, self.col - rows[k].0);
                spill = col == w;
                at = if spill { (row + 1, 0) } else { (row, col) };
            }
            out.extend(rows.into_iter().map(|(_, s)| s));
            if spill {
                out.push(String::new());
            }
        }
        (out, at)
    }

    /// ⇧↵ splits the line. Bare ↵ is left to the caller — in every box this
    /// lives in, it means "done", and a newline you have to ask for is cheaper
    /// than a submit you didn't.
    pub fn handle(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Char(c) => {
                let i = Self::byte_index(&self.lines[self.row], self.col);
                self.lines[self.row].insert(i, c);
                self.col += 1;
            }
            KeyCode::Enter if key.modifiers.intersects(newline_mods()) => {
                let i = Self::byte_index(&self.lines[self.row], self.col);
                let rest = self.lines[self.row].split_off(i);
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    self.col -= 1;
                    let i = Self::byte_index(&self.lines[self.row], self.col);
                    self.lines[self.row].remove(i);
                } else if self.row > 0 {
                    let cur = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = char_count(&self.lines[self.row]);
                    self.lines[self.row].push_str(&cur);
                }
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = char_count(&self.lines[self.row]);
                }
            }
            KeyCode::Right => {
                if self.col < char_count(&self.lines[self.row]) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up if self.row > 0 => {
                self.row -= 1;
                self.col = self.col.min(char_count(&self.lines[self.row]));
            }
            KeyCode::Down if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = self.col.min(char_count(&self.lines[self.row]));
            }
            _ => {}
        }
    }
}

/// green · red: an answer and a refusal. The common row.
pub const YES_NO: &[Color] = &[Color::Green, Color::Red];

/// Two ways forward, neither the "yes": green would invent a default answer.
pub const PEERS: &[Color] = &[Color::Gray, Color::Gray];

/// The action row every modal ends with. Index 0 is the one preselected, so the
/// common case is "↓ to the row, ↵"; destructive rows put `cancel` at 0 instead.
/// Each button also answers to one letter, picked out inside its label — see
/// `keys`.
pub struct Buttons {
    pub labels: &'static [&'static str],
    pub sel: usize,
    /// One colour per button, positional. Stated at the call site, because what
    /// a button *means* is only knowable there — short rows run out and fall
    /// back to grey.
    pub colors: &'static [Color],
}

impl Buttons {
    pub fn new(labels: &'static [&'static str], colors: &'static [Color]) -> Self {
        Self { labels, sel: 0, colors }
    }

    /// ←/→ (h/l) move; ↵ fires the selected one. Nothing else — a letter
    /// shortcut derived from the label only reads as one on some labels, and a
    /// key you have to guess at is worse than the two you can see.
    pub fn handle(&mut self, code: KeyCode) -> Option<usize> {
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.sel = self.sel.saturating_sub(1);
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.sel = (self.sel + 1).min(self.labels.len() - 1);
                None
            }
            KeyCode::Enter => Some(self.sel),
            _ => None,
        }
    }

    /// Each action as its own bordered button, side by side. 3 rows tall. No
    /// enclosing box and no label: two buttons explain themselves.
    ///
    /// The outline is always the action's own colour so the buttons read as
    /// controls even when the cursor is elsewhere; the armed one is filled
    /// edge to edge, which is what distinguishes it.
    pub fn render(&self, f: &mut Frame, area: Rect, focused: bool) {
        const GAP: u16 = 2;
        let w = self.labels.iter().map(|l| l.chars().count() as u16).max().unwrap_or(6) + 6;
        let mut x = area.x;
        for (i, label) in self.labels.iter().enumerate() {
            if x + w > area.x + area.width {
                break; // too narrow to draw the rest; nothing is clipped mid-border
            }
            let accent = self.colors.get(i).copied().unwrap_or(Color::Gray);
            let armed = focused && i == self.sel;
            let cell = Rect { x, y: area.y, width: w, height: 3.min(area.height) };
            let block = boxed("", Style::new()).border_style(Style::new().fg(accent));
            let inner = block.inner(cell);
            f.render_widget(block, cell);
            // Paragraph::style covers the whole inner rect; styling only the
            // Line paints behind the glyphs and leaves a half-filled button.
            let base = if armed {
                Style::new().bg(accent).fg(Color::Black).bold()
            } else {
                Style::new().fg(accent)
            };
            let text = Line::styled((*label).to_string(), base);
            f.render_widget(Paragraph::new(text).centered().style(base), inner);
            x += w + GAP;
        }
    }
}

/// `Scroll::by` as a key-handler arm: scrolling never navigates, so it always
/// answers `None`.
pub fn scrolled(s: &mut Scroll, d: i32) -> Option<Go> {
    s.by(d);
    None
}

/// Put text on the system clipboard. No dependency: every desktop ships a tool
/// that reads stdin, and the terminal is the wrong place to reimplement one.
/// Returns what to tell the human — success or the reason it couldn't.
pub fn clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    // ponytail: first tool that exists wins. OSC 52 would also work over ssh —
    // add it if anyone actually runs guvnor on a remote box.
    const TOOLS: [(&str, &[&str]); 4] = [
        ("pbcopy", &[]),                                   // macOS
        ("wl-copy", &[]),                                  // wayland
        ("xclip", &["-selection", "clipboard"]),           // x11
        ("xsel", &["--clipboard", "--input"]),             // x11, the other one
    ];
    for (bin, args) in TOOLS {
        let Ok(mut child) = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue; // not installed
        };
        let wrote = child
            .stdin
            .as_mut()
            .is_some_and(|s| s.write_all(text.as_bytes()).is_ok());
        drop(child.stdin.take());
        if wrote && child.wait().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
        return Err(format!("{bin} failed"));
    }
    Err("no clipboard tool found (pbcopy / wl-copy / xclip / xsel)".into())
}

/// A bare key press. Two test modules drive key handlers, and both need it.
#[cfg(test)]
pub fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hang_wrap_keeps_continuations_under_the_marker() {
        // "‣ " marker (width 2) + content long enough to wrap at 12 cols
        let line = Line::from(vec![
            Span::styled("‣ ", Style::new().fg(Color::DarkGray)),
            Span::raw("alpha beta gamma delta epsilon"),
        ]);
        let rows = hang_wrap(&line, 12);
        assert!(rows.len() >= 2, "should wrap: {rows:?}");
        let text = |l: &Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
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

    /// The reported bug: a long line ran off the right edge instead of
    /// continuing on the next row, so you could not see what you had typed.
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

}
