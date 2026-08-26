//! The Tests and Work tabs' bodies: a magit-style file list. One row per file,
//! `space` drops it open. The plumbing of a git patch (`index`, `---`, `+++`,
//! the `diff --git` line itself) says nothing the row doesn't, so none of it
//! reaches the screen.

use super::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::cell::{Cell, RefCell};

/// One collapsible row: a file's hunks, or the run's evidence.
pub struct Section {
    /// The row, without its ▸/▾ marker — `open` decides which is drawn.
    pub head: Line<'static>,
    pub body: Vec<Line<'static>>,
    pub open: bool,
    /// The body wrapped to a width, kept until the width changes. `hang_wrap`
    /// allocates per character, and both the renderer and `head_y` want the same
    /// answer, so recomputing it per frame and again per keypress burns real
    /// memory bandwidth on a list that has not changed.
    wrapped: RefCell<Option<(usize, Vec<Line<'static>>)>>,
}

impl Section {
    pub fn new(head: Line<'static>, body: Vec<Line<'static>>) -> Self {
        Self { head, body, open: false, wrapped: RefCell::new(None) }
    }

    /// The body wrapped to `w`, computed once per width.
    pub fn wrapped(&self, w: usize) -> std::cell::Ref<'_, Vec<Line<'static>>> {
        {
            let mut c = self.wrapped.borrow_mut();
            if c.as_ref().is_none_or(|(cached, _)| *cached != w) {
                *c = Some((w, hang_wrap_all(&self.body, w)));
            }
        }
        std::cell::Ref::map(self.wrapped.borrow(), |c| &c.as_ref().expect("just filled").1)
    }

    /// Rows this section draws when open, including its trailing blank.
    pub fn open_rows(&self, w: usize) -> u16 {
        u16::try_from(self.wrapped(w).len() + 1).unwrap_or(u16::MAX)
    }
}

/// A gate tab's diff: the files, a cursor, and the scroll they share.
#[derive(Default)]
pub struct DiffView {
    pub sections: Vec<Section>,
    pub sel: usize,
    pub scroll: Scroll,
    /// Rows the last frame had room for, so `handle` can scroll only when the
    /// cursor would otherwise leave the screen (the trick `Scroll::max` uses).
    pub view_h: Cell<u16>,
    /// Width the last frame wrapped body lines to, so `head_y` can reproduce
    /// the same wrapped row count `render_diff` actually drew.
    pub body_w: Cell<u16>,
}

impl DiffView {
    /// A gate tab: one section per file in `patch`, then the evidence — the
    /// thing that makes the patch worth anything — as one more collapsed row.
    pub fn build(
        patch: &std::path::Path,
        ev_head: Line<'static>,
        ev_body: Vec<Line<'static>>,
    ) -> Self {
        let raw = std::fs::read_to_string(patch).unwrap_or_default();
        let mut sections = patch_sections(&raw);
        if sections.is_empty() {
            sections.push(Section::new(
                Line::styled("(no patch recorded)", Style::new().fg(Color::DarkGray)),
                Vec::new(),
            ));
        }
        sections.push(Section::new(ev_head, ev_body));
        Self { sections, ..Default::default() }
    }

    /// Which drawn line section `i`'s row sits on: every row above it, plus the
    /// body (and its trailing blank) of any of them that is open.
    pub fn head_y(&self, i: usize) -> u16 {
        let mut y = 1usize; // the box's blank lead line
        let w = self.body_w.get().max(1) as usize;
        for s in self.sections.iter().take(i) {
            y += 1 + if s.open { s.open_rows(w) as usize } else { 0 };
        }
        u16::try_from(y).unwrap_or(u16::MAX)
    }

    /// Scroll only if the cursor would otherwise be off-screen.
    fn show_sel(&mut self) {
        let (y, h) = (self.head_y(self.sel), self.view_h.get().max(1));
        if y < self.scroll.off {
            self.scroll.off = y;
        } else if y >= self.scroll.off + h {
            self.scroll.off = y + 1 - h;
        }
    }

    /// True when the key was ours. `↵` is deliberately not: it judges the gate,
    /// and a key that means two things on one screen means neither.
    pub fn handle(&mut self, key: &KeyEvent) -> bool {
        if self.sections.is_empty() {
            return false;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(self.sections.len() - 1);
                self.show_sel();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                self.show_sel();
            }
            KeyCode::Char(' ') => {
                self.sections[self.sel].open = !self.sections[self.sel].open;
                self.show_sel();
            }
            KeyCode::PageDown => self.scroll.by(10),
            KeyCode::PageUp => self.scroll.by(-10),
            _ => return false,
        }
        true
    }
}

/// `modified  src/a.js  +12 -3`
fn head_line(path: &str, kind: &str, plus: usize, minus: usize) -> Line<'static> {
    let kstyle = match kind {
        "new" => Style::new().fg(Color::Green),
        "deleted" => Style::new().fg(Color::Red),
        _ => Style::new().fg(Color::Yellow),
    };
    Line::from(vec![
        Span::styled(format!("{kind:<9}"), kstyle),
        Span::styled(path.to_string(), Style::new().bold()),
        Span::styled(format!("  +{plus}"), Style::new().fg(Color::Green)),
        Span::styled(format!(" -{minus}"), Style::new().fg(Color::Red)),
    ])
}

/// Split a git patch into one section per file, dropping the plumbing.
pub fn patch_sections(patch: &str) -> Vec<Section> {
    // one file's worth of patch, while the split is in flight
    struct FileDiff {
        path: String,
        kind: &'static str,
        hunks: Vec<Line<'static>>,
        adds: usize,
        dels: usize,
    }
    let mut files: Vec<FileDiff> = Vec::new();
    for (l, in_hunk) in crate::worktree::patch_lines(patch) {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // `a/path b/path`; a placeholder good enough for a binary diff
            // (no `---`/`+++` pair follows those), overwritten below for the
            // common case where one does.
            let path = rest.rsplit(" b/").next().unwrap_or(rest).to_string();
            files.push(FileDiff { path, kind: "modified", hunks: Vec::new(), adds: 0, dels: 0 });
            continue;
        }
        // Anything before the first `diff --git` is git's own preamble.
        let Some(f) = files.last_mut() else { continue };
        // Header prefixes only mean anything outside a hunk; inside one they are
        // the human's own removed and added lines.
        if !in_hunk {
            if l.starts_with("new file") {
                f.kind = "new";
                continue;
            }
            if l.starts_with("deleted file") {
                f.kind = "deleted";
                continue;
            }
            // The authoritative path: each of these is a fixed prefix then the
            // path to end of line, so nothing in the path (a space, or literally
            // " b/") can be mistaken for the header's own syntax.
            if let Some(p) = l.strip_prefix("--- a/").or_else(|| l.strip_prefix("+++ b/")) {
                f.path = p.to_string();
            }
            // index / mode / rename headers and the ---,+++ pair: the row says
            // all of it (or it doesn't matter), and every one of them is a line
            // the human has to skip past to reach the change.
            if l.starts_with("index ")
                || l.starts_with("--- ")
                || l.starts_with("+++ ")
                || l.starts_with("old mode")
                || l.starts_with("new mode")
                || l.starts_with("similarity ")
                || l.starts_with("rename ")
            {
                continue;
            }
        }
        let style = match l.chars().next() {
            Some('@') => Style::new().fg(Color::Cyan),
            Some('+') => {
                f.adds += 1;
                Style::new().fg(Color::Green)
            }
            Some('-') => {
                f.dels += 1;
                Style::new().fg(Color::Red)
            }
            _ => Style::new().fg(Color::Gray),
        };
        f.hunks.push(Line::styled(format!("   {l}"), style));
    }
    files
        .into_iter()
        .map(|f| Section::new(head_line(&f.path, f.kind, f.adds, f.dels), f.hunks))
        .collect()
}

pub fn render_diff(f: &mut Frame, area: Rect, v: &DiffView) {
    v.view_h.set(area.height);
    v.body_w.set(area.width);
    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (i, s) in v.sections.iter().enumerate() {
        let sel = i == v.sel;
        let mut head = s.head.clone();
        head.spans.insert(
            0,
            Span::styled(
                if s.open { " ▾ " } else { " ▸ " },
                Style::new().fg(if sel { Color::Red } else { Color::DarkGray }),
            ),
        );
        // The cursor wears the same bar as the runs table: one plain highlight,
        // the row's own colours stood down while it is on.
        if sel {
            for sp in &mut head.spans {
                sp.style = sp.style.fg(Color::Black);
            }
            head = head.style(Style::new().bg(ART_WHITE).fg(Color::Black));
        }
        lines.push(head);
        if s.open {
            lines.extend(s.wrapped(area.width as usize).iter().cloned());
            lines.push(Line::raw(""));
        }
    }
    // Body lines wrap to the box width (`hang_wrap_all`), so a long code line
    // continues on the next row instead of running off the edge; `head_y`
    // wraps to the same width it last saw (`body_w`), so the drawn line count
    // and the cursor's row never disagree.
    let total = lines.len();
    let off = v.scroll.fit(total, area.height);
    f.render_widget(Paragraph::new(lines).scroll((off, 0)), area);
}
