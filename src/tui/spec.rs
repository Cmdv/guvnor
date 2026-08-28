//! The spec as boxes, and the popup that iterates it.
//!
//! A run has one screen; the spec is its first tab. Everything here is drawn
//! by `case.rs` — the panels on the Spec tab, the `Prompt` when you press `i`.

use crate::spec::Spec;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};
use ratatui::Frame;

use super::*;

/// Multiline prompt + action row (spec feedback). Tab moves between the text
/// and the row; ↵ in the text is a newline, ↵ on the row acts.
pub struct Prompt {
    pub text: TextArea,
    pub buttons: Buttons,
    pub on_buttons: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            text: TextArea::default(),
            buttons: Buttons::new(&["send", "cancel"], YES_NO),
            on_buttons: false,
        }
    }
}

/// Cursor and scroll offsets for the spec's six boxes. Every box is always on
/// screen; the one with the cursor is the one the arrows scroll, and its number
/// gets you there directly — that is how content taller than its box is read,
/// rather than by making the box taller than the screen.
#[derive(Default)]
pub struct SpecPanels {
    pub focus: usize,
    pub scrolls: [Scroll; 6],
}

impl SpecPanels {
    /// `1`-`6` jump, tab/backtab walk to the next/previous box, arrows scroll
    /// the box they land on. Returns whether the key was ours, so the caller
    /// can pass on the ones that aren't.
    pub fn handle(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c @ '1'..='6') => {
                self.focus = c as usize - '1' as usize;
                true
            }
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % 6;
                true
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + 5) % 6;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scrolls[self.focus].by(1);
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scrolls[self.focus].by(-1);
                true
            }
            KeyCode::PageDown => {
                self.scrolls[self.focus].by(10);
                true
            }
            KeyCode::PageUp => {
                self.scrolls[self.focus].by(-10);
                true
            }
            _ => false,
        }
    }
}

/// Which sections share a row. Two columns while there is room for them, one
/// when there isn't — the boxes survive the resize either way, because losing
/// them turned the spec back into the wall of text they exist to break up.
///
/// Wide: objective │ files · interfaces │ constraints, then verification
/// and the acceptance criteria on the full width. Those two are what the run is
/// judged against — one is the command that decides it, the other the numbered
/// sentences the reviewer scores — so neither gets squeezed into half a screen.
pub fn panel_rows(width: u16) -> Vec<Vec<usize>> {
    if width >= 88 {
        vec![vec![0, 1], vec![2, 3], vec![4], vec![5]]
    } else {
        (0..6).map(|i| vec![i]).collect()
    }
}

/// The spec as boxes. Every box is drawn, always: heights are shared out in
/// proportion to what each section needs, so nothing is hidden and no space is
/// wasted, and anything that still doesn't fit is scrolled inside its own box.
pub fn render_spec_panels(f: &mut Frame, area: Rect, sp: &Spec, p: &SpecPanels) {
    let s = spec_sections(sp);
    let rows = panel_rows(area.width);
    // Height each row would like: the tallest of its boxes, wrapped at the width
    // it will actually get. 2 border rows + 2 padding columns per box.
    let cell_w = area.width / rows.iter().map(|r| r.len()).max().unwrap_or(1) as u16;
    let need = |i: usize| {
        hang_wrap_all(&s[i].body, cell_w.saturating_sub(4).max(1) as usize).len() as u16 + 2
    };
    let mut weights: Vec<u16> =
        rows.iter().map(|r| r.iter().map(|&i| need(i)).max().unwrap_or(3).max(3)).collect();
    // The objective is the first thing anyone reads and it is prose, so give its
    // row a floor instead of only what its own line count asks for. Fill is
    // proportional, so the room comes off the tallest row — which is the
    // criteria list, capped here for the same reason. Neither hides anything:
    // every box has its own number and its own scroll.
    // Two constants because there are two rows that need one: the spec's
    // five parts are fixed by doctrine, not a list that grows.
    if let Some(w) = weights.first_mut() {
        *w = (*w).max(10);
    }
    if let Some(w) = weights.last_mut() {
        *w = (*w).min(6);
    }
    // Fill, not Length: when it all fits the boxes grow into the space instead
    // of leaving a gap at the bottom, and when it doesn't they shrink in
    // proportion rather than one of them disappearing.
    let areas = Layout::vertical(weights.iter().map(|w| Constraint::Fill(*w))).split(area);

    for (row, row_area) in rows.iter().zip(areas.iter()) {
        let cols = Layout::horizontal(vec![Constraint::Ratio(1, row.len() as u32); row.len()])
            .split(*row_area);
        for (&i, cell) in row.iter().zip(cols.iter()) {
            let focused = i == p.focus;
            let (border, text) = if focused {
                (Color::White, Style::new().fg(Color::White).bold())
            } else {
                (MODAL_BORDER, Style::new().fg(Color::Cyan).bold())
            };
            // The number is the key that gets you here, so it is red like every
            // other "press this" in the app; the brackets track the border, so a
            // focused panel lights them white too.
            let chrome = Style::new().fg(border);
            let title = Line::from(vec![
                Span::styled("─┐ ", chrome),
                Span::styled(format!("{}", i + 1), Style::new().fg(Color::Red).bold()),
                Span::styled(format!(" {} ", s[i].title), text),
                Span::styled(if focused { "↑↓ ┌" } else { "┌" }, chrome),
            ]);
            let block = boxed("", Style::new())
                .title(title)
                .border_style(Style::new().fg(border))
                .padding(Padding::horizontal(1));
            let inner = block.inner(*cell);
            let wrapped = hang_wrap_all(&s[i].body, inner.width.max(1) as usize);
            let off = p.scrolls[i].fit(wrapped.len(), inner.height);
            f.render_widget(Paragraph::new(wrapped).scroll((off, 0)).block(block), *cell);
        }
    }
}
