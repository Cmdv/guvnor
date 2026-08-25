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
    fn wrapped(&self, w: usize) -> std::cell::Ref<'_, Vec<Line<'static>>> {
        {
            let mut c = self.wrapped.borrow_mut();
            if c.as_ref().is_none_or(|(cached, _)| *cached != w) {
                *c = Some((w, hang_wrap_all(&self.body, w)));
            }
        }
        std::cell::Ref::map(self.wrapped.borrow(), |c| &c.as_ref().expect("just filled").1)
    }

    /// Rows this section draws when open, including its trailing blank.
    fn open_rows(&self, w: usize) -> u16 {
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
    view_h: Cell<u16>,
    /// Width the last frame wrapped body lines to, so `head_y` can reproduce
    /// the same wrapped row count `render_diff` actually drew.
    body_w: Cell<u16>,
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
    fn head_y(&self, i: usize) -> u16 {
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

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/a.js b/src/a.js
index d10a038..5886630 100644
--- a/src/a.js
+++ b/src/a.js
@@ -1,2 +1,2 @@
-old line
+new line
 context
diff --git a/test/b.test.js b/test/b.test.js
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/test/b.test.js
@@ -0,0 +1,2 @@
+one
+two
";

    /// The wrapped body is cached per width, so a resize has to invalidate it or
    /// the rows drawn and the cursor's idea of them drift apart.
    #[test]
    fn the_wrap_cache_follows_the_width() {
        let long = "x ".repeat(60);
        let s = Section::new(Line::raw("head"), vec![Line::raw(long)]);
        let wide = s.wrapped(120).len();
        let narrow = s.wrapped(40).len();
        assert!(narrow > wide, "narrower must wrap to more rows: {narrow} vs {wide}");
        // and coming back gives the same answer, not a stale one
        assert_eq!(s.wrapped(120).len(), wide);
        assert_eq!(s.open_rows(40) as usize, narrow + 1, "plus the trailing blank");
    }

    /// One row per file, the plumbing gone, and the counts taken from the hunks
    /// rather than from anything the model said.
    #[test]
    fn a_patch_becomes_one_row_per_file() {
        let s = patch_sections(PATCH);
        assert_eq!(s.len(), 2, "one section per file");
        assert_eq!(line_text(&s[0].head), "modified src/a.js  +1 -1");
        assert_eq!(line_text(&s[1].head), "new      test/b.test.js  +2 -0");
        let body: String = s.iter().flat_map(|f| f.body.iter()).map(line_text).collect();
        for noise in ["index ", "--- ", "+++ ", "diff --git"] {
            assert!(!body.contains(noise), "{noise:?} survived into the body: {body:?}");
        }
        assert!(body.contains("@@ -1,2 +1,2 @@"), "hunk headers stay: {body:?}");
        assert!(body.contains("+new line") && body.contains("-old line"));
        assert!(s.iter().all(|f| !f.open), "everything starts collapsed");
    }

    /// Verbatim `git diff --cached` output for deleting two `--` comments from a
    /// Haskell file (a shipped language preset). The `-` prefix makes each one
    /// `--- ...`, which is file-header syntax outside a hunk and the human's own
    /// deleted code inside one. Eat them and the work gate is judged on a diff
    /// that is missing lines, with a `-N` count that does not match.
    #[test]
    fn removed_comment_lines_are_not_mistaken_for_headers() {
        let patch = "\
diff --git a/Mean.hs b/Mean.hs
index 31d3074..efb9aa1 100644
--- a/Mean.hs
+++ b/Mean.hs
@@ -1,4 +1,2 @@
--- | Compute the mean
--- second comment
 mean :: [Double] -> Double
 mean xs = sum xs / n
";
        let s = patch_sections(patch);
        assert_eq!(s.len(), 1);
        assert_eq!(line_text(&s[0].head), "modified Mean.hs  +0 -2");
        let body: String = s[0].body.iter().map(line_text).collect();
        assert!(body.contains("-- | Compute the mean"), "removed line vanished: {body:?}");
        assert!(body.contains("-- second comment"), "removed line vanished: {body:?}");
    }

    /// Collapsed means collapsed: the tab opens as a list of what changed, and
    /// the hunks only reach the screen for the row you opened.
    #[test]
    fn the_list_draws_closed_and_opens_one_row_at_a_time() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut v = DiffView { sections: patch_sections(PATCH), ..Default::default() };
        let draw = |v: &DiffView| -> String {
            let mut t = Terminal::new(TestBackend::new(60, 12)).unwrap();
            t.draw(|f| render_diff(f, Rect::new(0, 0, 60, 12), v)).unwrap();
            screen_text(t.backend().buffer())
        };
        let closed = draw(&v);
        assert!(closed.contains("▸ modified src/a.js"), "{closed}");
        assert!(closed.contains("▸ new      test/b.test.js"), "{closed}");
        assert!(!closed.contains("new line"), "no hunks until you ask: {closed}");
        v.handle(&press(KeyCode::Char(' ')));
        let open = draw(&v);
        assert!(open.contains("▾ modified src/a.js"), "the marker turns: {open}");
        assert!(open.contains("+new line"), "and the hunk is there: {open}");
        assert!(open.contains("▸ new      test/b.test.js"), "the other stays shut: {open}");
    }

    /// The cursor walks the rows, space opens one, and an open row pushes the
    /// rows under it down — which is what `head_y` has to know for the scroll.
    #[test]
    fn space_opens_a_row_and_moves_the_ones_below_it() {
        let mut v = DiffView {
            sections: patch_sections(PATCH),
            ..Default::default()
        };
        // Built directly (no render_diff pass), so body_w defaults to 0; set
        // it wide enough that this fixture's body lines don't actually wrap,
        // so head_y still counts one row per body line below.
        v.body_w.set(200);
        assert_eq!(v.head_y(0), 1);
        assert_eq!(v.head_y(1), 2, "collapsed rows are one line each");
        assert!(v.handle(&press(KeyCode::Char(' '))));
        assert!(v.sections[0].open, "space opens the row under the cursor");
        // 4 body lines (@@, -, +, context) + a trailing blank
        assert_eq!(v.head_y(1), 7, "the row below is pushed past the body");
        assert!(v.handle(&press(KeyCode::Down)));
        assert_eq!(v.sel, 1);
        assert!(v.handle(&press(KeyCode::Down)));
        assert_eq!(v.sel, 1, "the cursor stops at the last row");
        // ↵ belongs to the gate, not to us
        assert!(!v.handle(&press(KeyCode::Enter)));
    }
}
