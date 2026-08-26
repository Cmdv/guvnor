//! The four hand-rolled controls the whole TUI is built from: a line of
//! text, a block of text, a row of buttons, and a scroll offset that knows where
//! its content ends.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::cell::Cell;

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
        // Saturate, don't truncate: a wrapped body past u16::MAX rows would wrap
        // to a near-zero ceiling and refuse to scroll at all.
        self.max.set(u16::try_from(content).unwrap_or(u16::MAX).saturating_sub(h));
        self.off.min(self.max.get())
    }
}

// ---- tiny line input: chars, backspace, arrows, ctrl+arrows by word ----

#[derive(Default)]
pub struct LineInput {
    pub value: String,
    pub cursor: usize, // char index
    pub max: usize,    // 0 = unlimited
}

/// The word boundary before `at`: back over any spaces, then back over the
/// word itself. Mirrored by `next_word` for the other direction.
fn prev_word(chars: &[char], at: usize) -> usize {
    let mut i = at;
    while i > 0 && chars[i - 1] == ' ' {
        i -= 1;
    }
    while i > 0 && chars[i - 1] != ' ' {
        i -= 1;
    }
    i
}

fn next_word(chars: &[char], at: usize) -> usize {
    let mut i = at;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    while i < chars.len() && chars[i] != ' ' {
        i += 1;
    }
    i
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Ctrl+letter arrives as Char('u') + CONTROL, so without the guard
            // the reflex for "kill this line" types a `u` into the field.
            KeyCode::Char(c) if !ctrl => {
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
            KeyCode::Left if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                self.cursor = prev_word(&chars, self.cursor);
            }
            KeyCode::Right if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                self.cursor = next_word(&chars, self.cursor);
            }
            KeyCode::Left if self.cursor > 0 => self.cursor -= 1,
            KeyCode::Right if self.cursor < self.value.chars().count() => self.cursor += 1,
            _ => {}
        }
    }
}

// ---- tiny multiline input: chars, backspace, arrows, shift to select ----

pub struct TextArea {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize, // char index
    /// The other end of a selection while shift extends one; `None` outside
    /// a selection.
    pub anchor: Option<(usize, usize)>,
    /// The width `render_textarea` last drew at, so `handle` can move the
    /// cursor by a wrapped row instead of a logical line — the same `Cell`
    /// trick `Scroll` uses: render knows the width, the key handler doesn't.
    /// Wide enough pre-render that nothing wraps, so ↑/↓ still walk logical
    /// lines until the first frame sets the real one.
    pub w: Cell<usize>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self { lines: vec![String::new()], row: 0, col: 0, anchor: None, w: Cell::new(9999) }
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

/// Horizontal scroll for a one-line input drawn in `w` columns: the column
/// offset that keeps the cursor on-screen, and the column the cursor lands on.
/// One definition, so the drawn slice and the cursor cannot disagree.
pub fn hscroll(cursor: usize, w: usize) -> (u16, u16) {
    let off = cursor.saturating_sub(w.saturating_sub(1));
    (off as u16, (cursor - off) as u16)
}

/// Draw a `TextArea` soft-wrapped in `area`, keeping the cursor row in view
/// and, while there's a selection, painting it in reverse video. Every
/// multiline input wants exactly this; none of them should own the
/// arithmetic.
pub fn render_textarea(f: &mut Frame, area: Rect, t: &TextArea, focused: bool) {
    let w = (area.width as usize).max(1);
    t.w.set(w);
    let (rows, (cr, cc)) = t.wrapped(w);
    let off = cr.saturating_sub(area.height.saturating_sub(1) as usize);
    let text: Vec<Line> = match t.anchor {
        Some(anchor) if anchor != (t.row, t.col) => {
            let a = t.wrap_pos(w, anchor.0, anchor.1);
            let (start, end) = if a <= (cr, cc) { (a, (cr, cc)) } else { ((cr, cc), a) };
            rows.iter().enumerate().map(|(r, s)| select_span(s, r, start, end)).collect()
        }
        _ => rows.iter().cloned().map(Line::raw).collect(),
    };
    f.render_widget(Paragraph::new(text).scroll((off as u16, 0)), area);
    if focused {
        f.set_cursor_position(ratatui::layout::Position::new(
            area.x + cc as u16,
            area.y + (cr - off) as u16,
        ));
    }
}

/// One wrapped row, with the `[start, end)` selection (in wrapped row/col)
/// picked out in reverse video.
fn select_span(s: &str, r: usize, start: (usize, usize), end: (usize, usize)) -> Line<'static> {
    if r < start.0 || r > end.0 {
        return Line::raw(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    let a = (if r == start.0 { start.1 } else { 0 }).min(chars.len());
    let b = (if r == end.0 { end.1 } else { chars.len() }).min(chars.len());
    let sel = Style::new().bg(Color::White).fg(Color::Black);
    let spans: Vec<Span> = [(&chars[..a], Style::new()), (&chars[a..b], sel), (&chars[b..], Style::new())]
        .into_iter()
        .filter(|(cs, _)| !cs.is_empty())
        .map(|(cs, st)| Span::styled(cs.iter().collect::<String>(), st))
        .collect();
    Line::from(spans)
}

impl TextArea {
    /// Seed it with existing text, cursor at the end — a draft you can edit,
    /// not a wall you have to retype.
    pub fn from(text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let lines = if lines.is_empty() { vec![String::new()] } else { lines };
        let row = lines.len() - 1;
        let col = char_count(&lines[row]);
        Self { lines, row, col, anchor: None, w: Cell::new(9999) }
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

    /// Where (row, col) sits among the wrapped rows at width `w` — the same
    /// mapping `wrapped` uses for the cursor, so a selection's other end
    /// lines up with what's drawn. Cruder than `wrapped`'s own cursor
    /// mapping (no spill row at an exact width boundary); close enough for a
    /// selection's far edge, which is never the cursor itself.
    fn wrap_pos(&self, w: usize, row: usize, col: usize) -> (usize, usize) {
        let w = w.max(1);
        let mut start = 0;
        for (r, line) in self.lines.iter().enumerate() {
            let rows = wrap_line(line, w);
            if r == row {
                let k = rows.iter().rposition(|(off, _)| *off <= col).unwrap_or(0);
                return (start + k, col - rows[k].0);
            }
            start += rows.len();
        }
        (start, 0)
    }

    /// Move by one wrapped row (the row the box actually draws) rather than
    /// one logical line, so ↑/↓ track what's on screen even once a line has
    /// wrapped.
    fn move_visual_row(&mut self, w: usize, d: i32) {
        let (wrow, wcol) = self.wrap_pos(w, self.row, self.col);
        let target = wrow as i32 + d;
        if target < 0 {
            return;
        }
        let mut acc = 0i32;
        for (r, line) in self.lines.iter().enumerate() {
            let rows = wrap_line(line, w);
            let n = rows.len() as i32;
            if target < acc + n {
                let (off, text) = &rows[(target - acc) as usize];
                self.row = r;
                self.col = off + wcol.min(char_count(text));
                return;
            }
            acc += n;
        }
        // Past the last row: rest at the very end of the text.
        self.row = self.lines.len() - 1;
        self.col = char_count(&self.lines[self.row]);
    }

    /// Remove the selection, if there is one, and leave the cursor where it
    /// started. Returns whether there was one to remove, so `Backspace` can
    /// skip its usual single-char delete when this already did the work.
    fn delete_selection(&mut self) -> bool {
        let Some(anchor) = self.anchor.take() else { return false };
        let (start, end) =
            if anchor <= (self.row, self.col) { (anchor, (self.row, self.col)) } else { ((self.row, self.col), anchor) };
        if start == end {
            return false;
        }
        let end_i = Self::byte_index(&self.lines[end.0], end.1);
        let tail = self.lines[end.0].split_off(end_i);
        let start_i = Self::byte_index(&self.lines[start.0], start.1);
        self.lines[start.0].truncate(start_i);
        self.lines[start.0].push_str(&tail);
        self.lines.drain(start.0 + 1..=end.0);
        self.row = start.0;
        self.col = start.1;
        true
    }

    /// ⇧↵ splits the line. Bare ↵ is left to the caller — in every box this
    /// lives in, it means "done", and a newline you have to ask for is cheaper
    /// than a submit you didn't.
    ///
    /// Shift+arrows extend a selection from wherever the cursor was when
    /// shift first went down; typing or deleting with one active replaces it,
    /// the same as any text box. Anything else collapses it — a moved cursor
    /// with no shift held means "start fresh", not "keep the old selection".
    pub fn handle(&mut self, key: &KeyEvent) {
        let arrow = matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down);
        if arrow && key.modifiers.contains(KeyModifiers::SHIFT) {
            self.anchor.get_or_insert((self.row, self.col));
        } else if arrow {
            self.anchor = None;
        }
        let w = self.w.get();
        match key.code {
            // Same as LineInput: a Ctrl+letter shortcut must not become text.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_selection();
                let i = Self::byte_index(&self.lines[self.row], self.col);
                self.lines[self.row].insert(i, c);
                self.col += 1;
            }
            KeyCode::Enter if key.modifiers.intersects(newline_mods()) => {
                self.delete_selection();
                let i = Self::byte_index(&self.lines[self.row], self.col);
                let rest = self.lines[self.row].split_off(i);
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Backspace => {
                if self.delete_selection() {
                    return;
                }
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
            KeyCode::Up => self.move_visual_row(w, -1),
            KeyCode::Down => self.move_visual_row(w, 1),
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
                self.prev();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.next();
                None
            }
            KeyCode::Enter => Some(self.sel),
            _ => None,
        }
    }

    /// Move the selection, clamped. A row with no labels is a real state (a
    /// committed run offers no actions), so the last index has to saturate:
    /// `len() - 1` on an empty list underflows.
    pub fn next(&mut self) {
        self.sel = (self.sel + 1).min(self.labels.len().saturating_sub(1));
    }

    pub fn prev(&mut self) {
        self.sel = self.sel.saturating_sub(1);
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

/// Which cell contains `pos`, if any. One hit-test for every clickable row
/// of rects: `tab_strip`'s cells today, `Buttons` and list rows next. Reuses
/// whatever geometry the render pass already produced.
pub fn hit_test(cells: &[Rect], pos: Position) -> Option<usize> {
    cells.iter().position(|r| r.contains(pos))
}

/// Put text on the system clipboard. No dependency: every desktop ships a
/// tool that reads stdin, and the terminal is the wrong place to reimplement
/// one. First tool that exists wins; with none installed, falls back to
/// OSC 52 — the terminal's own clipboard escape, and the one that still
/// works over ssh, since it asks whatever terminal is on the far end rather
/// than the box guvnor is actually running on.
/// Returns what to tell the human — success or the reason it couldn't.
pub fn clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
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
    let mut out = std::io::stdout();
    out.write_all(osc52_sequence(text, std::env::var_os("TMUX").is_some()).as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())
}

/// The OSC 52 escape that asks the terminal to set its own clipboard, base64
/// per the spec. Under tmux it has to be wrapped in a passthrough envelope
/// with its own escapes doubled, or tmux swallows it before it reaches the
/// real terminal.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> String {
    let payload = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    if in_tmux {
        format!("\x1bPtmux;{}\x1b\\", payload.replace('\x1b', "\x1b\x1b"))
    } else {
        payload
    }
}

/// RFC 4648 base64 with padding — just enough for OSC 52, not worth a
/// dependency for.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        out.push(T[(b[0] >> 2) as usize] as char);
        out.push(T[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 { T[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(b[2] & 0x3f) as usize] as char } else { '=' });
    }
    out
}

/// A bare key press. Two test modules drive key handlers, and both need it.
pub fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
}

/// A bare left-click at `(col, row)`. Mirrors `press` for mouse-driven tests.
pub fn click(col: u16, row: u16) -> ratatui::crossterm::event::MouseEvent {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    ratatui::crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// A rendered buffer as text, rows joined with newlines — the screen dump the
/// render tests grep. (`theme` and `text` are leaves and keep local copies.)
pub fn screen_text(buf: &ratatui::buffer::Buffer) -> String {
    let a = buf.area;
    (0..a.height)
        .map(|y| (0..a.width).map(|x| buf[(a.x + x, a.y + y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One styled line, flattened to its text.
pub fn line_text(l: &Line<'_>) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Styled lines flattened to text, one row per line.
pub fn lines_text(lines: &[Line<'_>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
}
