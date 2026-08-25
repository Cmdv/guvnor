//! The Review tab: what the reviewer flagged, what it said, what it cost,
//! and the two places an instruction about it can be sent.

use crate::review::Review;
use crate::state::{self, State};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Padding, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use super::*;

/// Ledger columns: lane · tokens in · tokens out · dollars. Fixed widths, so the
/// pane can be exactly as wide as the table needs and the prose beside it gets
/// every column that's left.
pub const COST_COLS: [Constraint; 4] = [
    Constraint::Length(16),
    Constraint::Length(7),
    Constraint::Length(7),
    Constraint::Length(7),
];

pub const COST_W: u16 = 16 + 7 + 7 + 7 + 3 + 2; // columns · spacing · border

/// Reviewer findings, triaged before the diffs are read: tick what should be
/// fixed and guvnor runs a fix round; leave them all and walk on to the case
/// file. Findings already fixed in an earlier round are shown green and inert.
pub struct ReviewView {
    pub id: String,
    /// Reviewer prose, pre-wrapped into readable paragraphs.
    pub summary: Vec<Line<'static>>,
    /// Per-lane spend ledger (spec draft, spec revisions, review rounds, fixes),
    /// structured so it can be a real table with a header that stays put.
    pub cost: Vec<crate::casefile::CostRow>,
    /// Column sums, drawn as the table's footer: the total price is the number
    /// you must never have to scroll to find.
    pub cost_total: (u64, u64, f64),
    /// The decision + provenance, drawn last: it's the conclusion, not the lede.
    pub verdict: Line<'static>,
    /// One-glyph decision for the tab label.
    pub mark: Span<'static>,
    pub live: Vec<crate::review::Finding>,
    pub checked: Vec<bool>,
    /// This finding matches one a previous fix round already addressed — the
    /// reviewer raised it again, which is the opposite of resolved.
    pub reraised: Vec<bool>,
    pub resolved: Vec<crate::review::Finding>,
    /// Your own instruction, sent to the fix lane with the ticked findings —
    /// one line, like a commit subject: enough for an instruction, not an essay.
    pub note: LineInput,
    /// Cursor: `0..live.len()` findings · then the instruction · then the actions.
    pub sel: usize,
    pub buttons: Buttons,
    /// Which section `tab` has focus on. Only the findings section takes a
    /// cursor; the other two are read-only panes that scroll.
    pub focus: ReviewFocus,
    pub summary_scroll: Scroll,
    pub cost_scroll: Scroll,
    /// The landing surface, as a box at the foot of this tab. `Some` once every
    /// gate is green (ready to stage), `None` while it is still muted — you can
    /// focus it either way (`s`), but only a `Some` one has buttons that fire.
    pub stage: Option<StageView>,
}

/// Sections of the review screen, cycled with `tab`. Boxes that can hold more
/// than they show need a way to be scrolled, and a way to see which one the
/// arrows will move. `Stage` is the landing box at the foot of the tab — `s`
/// jumps straight to it.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ReviewFocus {
    Findings,
    Summary,
    Cost,
    Stage,
}

impl ReviewFocus {
    pub fn next(self) -> Self {
        match self {
            ReviewFocus::Findings => ReviewFocus::Summary,
            ReviewFocus::Summary => ReviewFocus::Cost,
            ReviewFocus::Cost => ReviewFocus::Stage,
            ReviewFocus::Stage => ReviewFocus::Findings,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ReviewFocus::Findings => ReviewFocus::Stage,
            ReviewFocus::Stage => ReviewFocus::Cost,
            ReviewFocus::Cost => ReviewFocus::Summary,
            ReviewFocus::Summary => ReviewFocus::Findings,
        }
    }
}

impl ReviewView {
    pub fn note_row(&self) -> usize {
        self.live.len()
    }

    pub fn action_row(&self) -> usize {
        self.live.len() + 1
    }

    /// The scroll offset the arrows currently drive, or `None` on the findings
    /// section, where they move a cursor instead.
    pub fn scroll(&mut self) -> Option<&mut Scroll> {
        match self.focus {
            // Findings walks a cursor; Stage walks its buttons and scrolls its
            // file list with PgUp/PgDn — neither is driven by the shared arrows.
            ReviewFocus::Findings | ReviewFocus::Stage => None,
            ReviewFocus::Summary => Some(&mut self.summary_scroll),
            ReviewFocus::Cost => Some(&mut self.cost_scroll),
        }
    }
}

/// What the Review tab did with a key. `No` matters: the tab strip is shared
/// with the other tabs, and ←/→ have to keep moving between them.
pub enum Took {
    No,
    Yes,
    Say(&'static str),
    Go(Go),
}

/// Keys for the Review tab. Cursor walks the findings, then the instruction
/// line, then the action row: ↵ ticks a finding · leaves the instruction · fires
/// the fix round. `tab` moves between the three sections; on the two read-only
/// ones the arrows scroll instead of moving a cursor.
pub fn review_key(r: &mut ReviewView, key: &KeyEvent) -> Took {
    match key.code {
        KeyCode::Tab => {
            r.focus = r.focus.next();
            return Took::Yes;
        }
        KeyCode::BackTab => {
            r.focus = r.focus.prev();
            return Took::Yes;
        }
        _ => {}
    }
    // Jump straight to a section by its red letter — everywhere except the
    // instruction line, where the letters are text. The buttons take no letters,
    // so nothing else competes for them.
    let typing = r.focus == ReviewFocus::Findings && r.sel == r.note_row();
    if !typing {
        let jump = match key.code {
            KeyCode::Char('f') => Some(ReviewFocus::Findings),
            KeyCode::Char('r') => Some(ReviewFocus::Summary),
            KeyCode::Char('t') => Some(ReviewFocus::Cost),
            KeyCode::Char('s') => Some(ReviewFocus::Stage),
            _ => None,
        };
        if let Some(to) = jump {
            r.focus = to;
            return Took::Yes;
        }
    }
    // The stage box has its own controls: ↑/↓ walk the buttons, PgUp/PgDn scroll
    // the file list, ↵ fires. Muted (no StageView) it swallows nothing, so ←/→
    // still leave. The actions rebuild the run, which needs `App`, so ↵ hands a
    // `Go` back rather than acting here.
    if r.focus == ReviewFocus::Stage {
        let Some(stage) = r.stage.as_mut() else { return Took::No };
        return match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                stage.buttons.next();
                Took::Yes
            }
            KeyCode::Up | KeyCode::Char('k') => {
                stage.buttons.prev();
                Took::Yes
            }
            KeyCode::PageDown => {
                stage.scroll.by(5);
                Took::Yes
            }
            KeyCode::PageUp => {
                stage.scroll.by(-5);
                Took::Yes
            }
            // The tree's state names the action at each index — `StageView::build`
            // laid the buttons out from the same state, so the labels stay
            // display-only. Committed offers nothing to fire.
            KeyCode::Enter => match (stage.done, stage.staged, stage.buttons.sel) {
                (true, ..) => Took::Yes,
                (_, false, _) => Took::Go(Go::Stage(r.id.clone())),
                (_, true, 0) => Took::Go(Go::OpenCommit(r.id.clone())),
                (_, true, _) => Took::Go(Go::Unstage(r.id.clone())),
            },
            // ←/→ (h/l) belong to the tab strip, always
            _ => Took::No,
        };
    }
    if let Some(scroll) = r.scroll() {
        let d = match key.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown => 5,
            KeyCode::PageUp => -5,
            _ => return Took::No,
        };
        scroll.by(d);
        return Took::Yes;
    }
    // The instruction line is a text field: letters type (so no j/k cursor
    // here), the bare arrows and ↵ are the way out.
    //
    // ←/→ only belong to it while there is text to move through. With no
    // findings the cursor starts here, and swallowing them made the Review tab
    // one you could not arrow out of — the exact trap the tab strip must never
    // be in.
    if r.sel == r.note_row() {
        let sideways = matches!(key.code, KeyCode::Left | KeyCode::Right);
        if sideways && r.note.value.is_empty() {
            return Took::No;
        }
        match key.code {
            KeyCode::Down | KeyCode::Enter => r.sel = r.action_row(),
            KeyCode::Up => r.sel = r.sel.saturating_sub(1),
            _ => r.note.handle(key),
        }
        return Took::Yes;
    }
    match key.code {
        // On the action row ↓ keeps walking: along the buttons, then off the end
        // and back to the top of the list. One key reaches everything; ←/→
        // still work; ↓ also reaches the second button.
        KeyCode::Down | KeyCode::Char('j') if r.sel == r.action_row() => {
            if r.buttons.sel + 1 < r.buttons.labels.len() {
                r.buttons.sel += 1;
            } else {
                r.buttons.sel = 0;
                r.sel = 0;
            }
            Took::Yes
        }
        KeyCode::Up | KeyCode::Char('k') if r.sel == r.action_row() && r.buttons.sel > 0 => {
            r.buttons.sel -= 1;
            Took::Yes
        }
        KeyCode::Down | KeyCode::Char('j') => {
            r.sel = (r.sel + 1).min(r.action_row());
            Took::Yes
        }
        KeyCode::Up | KeyCode::Char('k') => {
            r.sel = r.sel.saturating_sub(1);
            Took::Yes
        }
        KeyCode::Enter if r.sel < r.live.len() => {
            r.checked[r.sel] = !r.checked[r.sel];
            Took::Yes
        }
        // ←/→ (and h/l) are the tab strip's, so they never reach `Buttons` —
        // otherwise leaving the tab would quietly re-arm the other action.
        _ if r.sel == r.action_row()
            && matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l')
            ) =>
        {
            Took::No
        }
        _ if r.sel == r.action_row() => match r.buttons.handle(key.code) {
            // The implementer, working inside the spec.
            Some(0) => {
                let picked: Vec<_> = r
                    .live
                    .iter()
                    .zip(&r.checked)
                    .filter(|(_, c)| **c)
                    .map(|(f, _)| f.clone())
                    .collect();
                let note = r.note.value.trim().to_string();
                if picked.is_empty() && note.is_empty() {
                    Took::Say("tick a finding or type an instruction first")
                } else {
                    Took::Go(Go::Fix(r.id.clone(), picked, note))
                }
            }
            // The planner. Anything that changes what the feature IS — dropping
            // a file the spec requires, removing tests — is a spec change, and
            // the implementer is fenced out of making it. Same words you already
            // typed, sent to the lane that can act on them.
            Some(_) => match r.note.value.trim() {
                "" => Took::Say("type what the spec should say instead"),
                note => Took::Go(Go::Replan(r.id.clone(), note.to_string())),
            },
            // ↑/↓ walk this row, so ←/→ stay the tab keys here too. Inside the
            // Review tab they mean one thing only: change tab.
            None => Took::No,
        },
        _ => Took::No,
    }
}

/// The Review tab: what was flagged (with the controls to act on it), then
/// the reviewer's reasoning and the spend side by side — two short, unrelated
/// things side by side, so each gets the height it needs — and the decision
/// last, as a conclusion.
pub fn render_review_tab(f: &mut Frame, area: Rect, v: &ReviewView) {
    // The comment and the ledger take what their content needs and never more
    // than half the screen — they are reference material, and the findings are
    // the thing you act on. Everything left over goes to the findings.
    let panes_need = {
        let prose = v.summary.len() as u16 + 2;
        let ledger = if v.cost.is_empty() { 3 } else { v.cost.len() as u16 + 4 };
        prose.max(ledger)
    };
    // The stage box sits at the foot of the tab: its content when ready, one
    // line when muted. It comes out of the panes' half, never the findings', so
    // the thing you act on keeps its room.
    // +1 on the border rows for the blank first line every box carries.
    let stage_h = match &v.stage {
        None => 4,
        Some(s) => {
            let btn = if s.buttons.labels.is_empty() { 0 } else { 3 };
            let files = (s.files.len() as u16).clamp(1, 3);
            (files + s.explain().len() as u16 + btn + 3).min(area.height / 3)
        }
    };
    // Findings is the thing you act on: give it its 8 rows and the stage box its
    // height first, then the reference panes take what's left (never below 6).
    let panes_h = {
        let cap = area.height.saturating_sub(4 + stage_h + 8).max(6);
        panes_need.clamp(6, cap)
    };
    let [verdict_a, find_a, panes_a, stage_a] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(panes_h),
        Constraint::Length(stage_h),
    ])
    .areas(area);
    f.render_widget(
        Paragraph::new(v.verdict.clone())
            .block(boxed("the reviewer's verdict", Style::new().bold())
                .border_style(Style::new().fg(MODAL_BORDER))
                .padding(Padding::new(0, 0, 1, 0))),
        verdict_a,
    );
    // The ledger needs 4 narrow columns; the prose takes what's left.
    let [sum_a, cost_a] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(COST_W)]).areas(panes_a);

    // findings box holds the whole control surface: the list, then an
    // unnamed instruction box, then the button — you never leave it
    let picked = v.checked.iter().filter(|c| **c).count();
    let on_findings = v.focus == ReviewFocus::Findings;
    let find_box = focus_box(
        "f",
        &format!("findings — {picked} of {} ticked to fix", v.live.len()),
        on_findings,
    );
    let find_inner = find_box.inner(find_a);
    f.render_widget(find_box, find_a);
    let [top_a, note_a, btn_a] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(4), Constraint::Length(3)])
            .areas(find_inner);
    // What each finding IS on the left, what the reviewer said about it on the
    // right. Side by side the list stays a list. Each gets its own frame: two
    // columns with a rule between them read as one block of text.
    let [list_a, why_a] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Min(24)]).areas(top_a);
    let frame = || {
        boxed("", Style::new())
            .border_style(Style::new().fg(MODAL_BORDER))
            .padding(Padding::new(0, 0, 1, 0))
    };
    let list_inner = frame().inner(list_a);
    f.render_widget(frame(), list_a);

    let mut list: Vec<Line> = Vec::new();
    if v.live.is_empty() {
        list.push(Line::styled(
            "  no findings — the reviewer raised nothing",
            Style::new().fg(Color::Green),
        ));
    }
    for (i, fd) in v.live.iter().enumerate() {
        let sel = on_findings && i == v.sel;
        let mut spans = vec![
            Span::raw(if sel { " ▸ " } else { "   " }),
            Span::styled(
                if v.checked[i] { "[x] " } else { "[ ] " },
                Style::new().fg(if v.checked[i] { Color::Yellow } else { Color::DarkGray }),
            ),
            Span::styled(format!("{:<7}", fd.severity.to_string()), severity_style(fd.severity)),
            Span::styled(
                if fd.file.is_empty() { "—".into() } else { fd.file.clone() },
                Style::new().fg(Color::Cyan),
            ),
        ];
        if v.reraised[i] {
            spans.push(Span::styled(" ↻", Style::new().fg(Color::Red).bold()));
        }
        let mut row = Line::from(spans);
        if sel {
            row = row.style(Style::new().bold());
        }
        list.push(row);
    }
    if !v.resolved.is_empty() {
        list.push(Line::raw(""));
        list.push(rule("already fixed", Color::Green));
        for fd in &v.resolved {
            list.push(Line::styled(
                format!("   ✓ {:<7}{}", fd.severity, if fd.file.is_empty() { "—" } else { &fd.file }),
                Style::new().fg(Color::Green),
            ));
        }
    }
    // One row per finding, so the cursor's row IS its index — just keep it on
    // screen.
    // `sel` also addresses the instruction box and the button row below the
    // list, which are not findings: scrolling to those would drag the list past
    // its last real row and take the finding being read off the top.
    let last = v.live.len().saturating_sub(1);
    let yoff = (v.sel.min(last) as u16).saturating_sub(list_inner.height.saturating_sub(1));
    f.render_widget(Paragraph::new(list).scroll((yoff, 0)), list_inner);

    // The reason, for whichever finding the cursor is on — its own box, titled
    // with the file it is about, so the body is only the reviewer's words.
    let (title, style, why) = match v.live.get(v.sel) {
        Some(fd) => {
            let mut lines = Vec::new();
            if v.reraised[v.sel] {
                lines.push(Line::styled("↻ raised again after a fix", Style::new().fg(Color::Red)));
                lines.push(Line::raw(""));
            }
            lines.push(Line::raw(fd.note.clone()));
            let file = if fd.file.is_empty() { "—".to_string() } else { fd.file.clone() };
            (file, severity_style(fd.severity).bold(), lines)
        }
        None => (
            String::new(),
            Style::new(),
            vec![Line::styled("↑↓ over a finding to read why", Style::new().fg(Color::DarkGray))],
        ),
    };
    f.render_widget(
        Paragraph::new(why).wrap(Wrap { trim: false }).block(
            boxed(&title, style)
                .border_style(Style::new().fg(MODAL_BORDER))
                .padding(Padding::horizontal(1)),
        ),
        why_a,
    );

    // unnamed: it sits under the findings it belongs to
    let on_note = on_findings && v.sel == v.note_row();
    let note_box = boxed("", Style::new())
        .border_style(Style::new().fg(if on_note { Color::White } else { MODAL_BORDER }));
    let note_inner = note_box.inner(note_a);
    f.render_widget(note_box, note_a);
    let (xoff, cx) = hscroll(v.note.cursor, note_inner.width as usize);
    let hint = if v.note.value.is_empty() && !on_note {
        Paragraph::new(Line::styled(
            " optional: tell the fix lane anything else",
            Style::new().fg(Color::DarkGray),
        ))
    } else {
        Paragraph::new(v.note.value.as_str()).scroll((0, xoff))
    };
    f.render_widget(hint, note_inner);
    if on_note {
        f.set_cursor_position(Position::new(note_inner.x + cx, note_inner.y));
    }
    v.buttons.render(f, btn_a, on_findings && v.sel == v.action_row());

    let sum_box = focus_box("r", "reviewer comment", v.focus == ReviewFocus::Summary);
    let sum_inner = sum_box.inner(sum_a);
    let prose = Paragraph::new(v.summary.clone()).wrap(Wrap { trim: false });
    let off = v.summary_scroll.fit(prose.line_count(sum_inner.width), sum_inner.height);
    f.render_widget(prose.scroll((off, 0)).block(sum_box), sum_a);

    render_cost(f, cost_a, v);
    render_stage_box(f, stage_a, v);
}

/// The landing box at the foot of the Review tab. Greyed and inert until every
/// gate is green (`v.stage` is `None`), then it lists the files, says what will
/// happen, and offers the moves the tree is in a state for. `s` focuses it.
pub fn render_stage_box(f: &mut Frame, area: Rect, v: &ReviewView) {
    let focused = v.focus == ReviewFocus::Stage;
    // Lowercase title so `focus_box`'s red-key highlighter (`title.find("s")`)
    // lands the `s` on "stage", the way f/r/t do on their sibling boxes.
    let Some(s) = &v.stage else {
        // Muted: visible so you know landing is coming, greyed so you know it
        // isn't yet. `focus_box(.., false)` keeps it grey even when focused.
        let block = focus_box("s", "stage — locked until every gate is green", false)
            .padding(Padding::new(0, 0, 1, 0));
        f.render_widget(
            Paragraph::new(Line::styled(
                " approve Spec · Tests · Work to land — each gate gates the commit",
                Style::new().fg(Color::DarkGray),
            ))
            .block(block),
            area,
        );
        return;
    };
    let state = if s.done {
        "committed".to_string()
    } else if s.staged {
        "staged, uncommitted".to_string()
    } else {
        format!("{} file(s) ready", s.files.len())
    };
    let block = focus_box("s", &format!("stage — {state}"), focused).padding(Padding::new(0, 0, 1, 0));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let btn_h = if s.buttons.labels.is_empty() { 0 } else { 3 };
    let words = s.explain();
    let [files_a, what_a, btn_a] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(words.len() as u16),
        Constraint::Length(btn_h),
    ])
    .areas(inner);
    let lines: Vec<Line> = s
        .files
        .iter()
        .map(|p| {
            Line::from(vec![
                Span::styled(" ‣ ", Style::new().fg(Color::DarkGray)),
                Span::styled(p.clone(), Style::new().fg(Color::Cyan)),
            ])
        })
        .collect();
    let off = s.scroll.fit(lines.len(), files_a.height);
    f.render_widget(Paragraph::new(lines).scroll((off, 0)), files_a);
    f.render_widget(Paragraph::new(words).wrap(Wrap { trim: false }), what_a);
    if btn_h > 0 {
        s.buttons.render(f, btn_a, focused);
    }
}

/// What the word actually means, next to the word. `WARNING` on its own reads as
/// "something is broken"; it means the opposite — it works, it is not what you
/// asked for. Nobody should have to know the reviewer's prompt to land a run.
pub fn verdict_means(d: crate::review::Decision) -> &'static str {
    match d {
        crate::review::Decision::Approved => "— every criterion met, nothing to flag",
        crate::review::Decision::Warning => {
            "— criteria met, but the findings below are worth reading first"
        }
        crate::review::Decision::Blocked => "— a criterion is unmet, or the diff went past the spec",
    }
}

/// The spend ledger as a real table: the column headings stay put and the
/// total is the table's footer, so the one number you always want is never the
/// one that scrolled away. `tok` is in the heading, not on every row; money is
/// in dollars to the cent, because fractions of a cent are not a decision.
pub fn render_cost(f: &mut Frame, area: Rect, v: &ReviewView) {
    let dim = Style::new().fg(Color::DarkGray);
    let block =
        focus_box("t", "tokens / cost", v.focus == ReviewFocus::Cost).padding(Padding::new(0, 0, 1, 0));
    if v.cost.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(" no metrics recorded", dim)).block(block),
            area,
        );
        return;
    }
    let inner = block.inner(area);
    // header and footer are outside the scrolling body
    let body_h = inner.height.saturating_sub(2);
    let mut state = TableState::new().with_offset(v.cost_scroll.fit(v.cost.len(), body_h) as usize);
    let (tin, tout, total) = v.cost_total;
    let table = Table::new(
        v.cost.iter().map(|r| {
            Row::new(vec![
                Cell::from(r.name.clone()),
                Cell::from(Line::from(crate::casefile::fmt_tok(r.tin)).right_aligned()),
                Cell::from(Line::from(crate::casefile::fmt_tok(r.tout)).right_aligned()),
                Cell::from(Line::from(format!("${:.2}", r.cost)).right_aligned()),
            ])
        }),
        COST_COLS,
    )
    .header(
        Row::new([
            Cell::from("lane"),
            Cell::from(Line::from("in").right_aligned()),
            Cell::from(Line::from("out").right_aligned()),
            Cell::from(Line::from("cost").right_aligned()),
        ])
        .style(dim.underlined().bold()),
    )
    .footer(
        Row::new(vec![
            Cell::from("total"),
            Cell::from(Line::from(crate::casefile::fmt_tok(tin)).right_aligned()),
            Cell::from(Line::from(crate::casefile::fmt_tok(tout)).right_aligned()),
            Cell::from(Line::from(format!("${total:.2}")).right_aligned()),
        ])
        .style(Style::new().fg(Color::Yellow).bold()),
    )
    .column_spacing(1)
    .block(block);
    f.render_stateful_widget(table, area, &mut state);
}

impl App {

    /// The Review tab's contents. `None` when the run was never reviewed — a
    /// missing or unreadable review.json is a legitimate state, not an error.
    pub fn build_review(&self, dir: &std::path::Path, st: &State) -> Option<ReviewView> {
        let raw = std::fs::read_to_string(dir.join("review.json")).ok()?;
        let r: Review = serde_json::from_str(&raw).ok()?;

        let summary = if r.verdict.summary.trim().is_empty() {
            vec![Line::styled("(the reviewer wrote no summary)", Style::new().fg(Color::DarkGray))]
        } else {
            paragraphs(&r.verdict.summary)
        };
        let cost = crate::casefile::cost_rows(dir);
        let cost_total = crate::casefile::cost_total(&cost);
        let decision = r.verdict.verdict.to_string();
        // The verdict is a box of its own above the findings: the word, what it
        // means, and the provenance that makes it checkable.
        let verdict = Line::from(vec![
            Span::raw(" "),
            verdict_span(&decision).bold(),
            Span::styled(format!("  {}", verdict_means(r.verdict.verdict)), Style::new()),
            Span::styled(
                // chars, not bytes: review.json is read back off disk without
                // validation, and slicing a multibyte field mid-character panics
                // the whole screen.
                format!(
                    "   {} · {} · diff {}",
                    r.model,
                    r.ts,
                    r.diff_sha256.chars().take(12).collect::<String>()
                ),
                Style::new().fg(Color::DarkGray),
            ),
        ]);
        // The tab label carries the decision, so the strip answers "did it pass?"
        // without opening the tab. Never a ✓: that glyph means "you approved
        // this gate" on the three tabs beside it, and a reviewer approves
        // nothing. The triangle just takes the verdict's colour.
        let mark = Span::styled("▲ ", verdict_span(&decision).style);

        let done: Vec<String> = st.fixed_findings.iter().map(state::finding_key).collect();
        let live = r.verdict.findings;
        Some(ReviewView {
            id: st.id.clone(),
            summary,
            cost,
            cost_total,
            verdict,
            mark,
            reraised: live.iter().map(|f| done.contains(&state::finding_key(f))).collect(),
            checked: vec![false; live.len()],
            live,
            resolved: st.fixed_findings.clone(),
            note: LineInput::default(),
            sel: 0,
            // Two destinations for one instruction box: the implementer for a
            // change inside the spec, the planner for a change to the spec.
            // Peers, not an answer and a refusal — green on one of them would
            // invent a default that doesn't exist.
            buttons: Buttons::new(&["fix the code", "change the spec"], PEERS),
            focus: ReviewFocus::Findings,
            summary_scroll: Scroll::default(),
            cost_scroll: Scroll::default(),
            // Attached by `build_case` once every gate is green; muted until then.
            stage: None,
        })
    }

}

#[cfg(test)]
impl ReviewView {
    /// A `ReviewView` with `n` findings and an optional stage box — enough to
    /// drive keys and rendering without a review.json on disk.
    pub fn stub(n: usize, stage: Option<StageView>) -> Self {
        ReviewView {
            id: "r".into(),
            summary: Vec::new(),
            cost: Vec::new(),
            cost_total: (0, 0, 0.0),
            verdict: Line::raw(""),
            mark: Span::raw(""),
            live: (0..n)
                .map(|i| crate::review::Finding {
                    severity: crate::review::Severity::Medium,
                    file: format!("f{i}"),
                    note: "n".into(),
                })
                .collect(),
            checked: vec![false; n],
            reraised: vec![false; n],
            resolved: Vec::new(),
            note: LineInput::default(),
            sel: 0,
            buttons: Buttons::new(&["fix the code", "change the spec"], PEERS),
            focus: ReviewFocus::Findings,
            summary_scroll: Scroll::default(),
            cost_scroll: Scroll::default(),
            stage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ReviewView` with `n` findings and no stage box — enough to drive keys.
    fn review_stub(n: usize) -> ReviewView {
        ReviewView::stub(n, None)
    }


    #[test]
    fn review_tab_takes_its_own_keys_but_never_traps_the_tab_strip() {
        let mut r = review_stub(2);
        // ←/→ are the run screen's tab keys everywhere else: on a finding row
        // the review must hand them back, or Review becomes a tab you can't leave
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char('h'), KeyCode::Char('l')] {
            assert!(matches!(review_key(&mut r, &press(code)), Took::No), "{code:?} must pass through");
        }
        // any unbound letter passes through
        assert!(matches!(review_key(&mut r, &press(KeyCode::Char('m'))), Took::No));

        // ↵ on a finding ticks it
        assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Yes));
        assert!(r.checked[0]);

        // the action row: nothing ticked and nothing typed says so instead of
        // launching an empty job
        r.sel = r.action_row();
        r.checked[0] = false;
        assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Say(_)));
        r.checked[0] = true;
        assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Go(Go::Fix(..))));

        // an instruction the implementer can't act on goes to the planner —
        // the second button is the only way out of "CANNOT: contradicts the spec"
        review_key(&mut r, &press(KeyCode::Down)); // ↓ walks the row, not ←/→
        assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Say(_)), "no words to send");
        r.note.value = "drop the LICENSE file".into();
        assert!(matches!(review_key(&mut r, &press(KeyCode::Enter)), Took::Go(Go::Replan(..))));
        // ...and ←/→ on the action row still belong to the tab strip
        assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::No));
        r.note.value.clear();

        // on the instruction line letters type instead of moving the cursor
        r.sel = r.note_row();
        review_key(&mut r, &press(KeyCode::Char('j')));
        assert_eq!(r.note.value, "j");
        assert_eq!(r.sel, r.note_row(), "typing must not move off the field");
        // ←/→ move the text cursor only while there is text: an empty field is
        // not worth trapping the tab keys for
        assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::Yes));
        r.note.value.clear();
        assert!(matches!(review_key(&mut r, &press(KeyCode::Left)), Took::No));
        review_key(&mut r, &press(KeyCode::Down));
        assert_eq!(r.sel, r.action_row(), "the bare arrow is the way out");

        // off the findings, the arrows scroll the pane `tab` selected
        r.focus = ReviewFocus::Cost;
        r.cost_scroll.max.set(9);
        r.note.value = "keep me".into();
        review_key(&mut r, &press(KeyCode::Down));
        assert_eq!(r.cost_scroll.off, 1);
        assert_eq!(r.note.value, "keep me", "scrolling must not reach the text field");
    }

    /// Reported: on a review with no findings the cursor starts on the
    /// instruction line, and ←/→ disappeared into an empty text field — so the
    /// tab strip was unreachable and the tab could not be left.
    #[test]
    fn an_empty_review_is_not_a_tab_you_get_stuck_in() {
        let mut r = review_stub(0);
        assert_eq!(r.sel, r.note_row(), "nothing to tick: the cursor starts on the field");
        for code in [KeyCode::Left, KeyCode::Right] {
            assert!(
                matches!(review_key(&mut r, &press(code)), Took::No),
                "{code:?} must reach the tab strip"
            );
        }
        // and from the action row
        r.sel = r.action_row();
        assert!(matches!(review_key(&mut r, &press(KeyCode::Right)), Took::No));
        // ↓ is what walks the buttons
        assert!(matches!(review_key(&mut r, &press(KeyCode::Down)), Took::Yes));
        assert_eq!(r.buttons.sel, 1);
    }

    #[test]
    fn cost_header_and_total_stay_put_while_the_ledger_scrolls() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut v = review_stub(0);
        v.cost = (0..20)
            .map(|i| crate::casefile::CostRow {
                name: format!("lane{i}"),
                tin: 1000,
                tout: 100,
                cost: 0.01,
            })
            .collect();
        v.cost_total = crate::casefile::cost_total(&v.cost);
        // 8 rows: border, blank (the box's lead line), header, 3 body rows,
        // footer, border
        let mut t = Terminal::new(TestBackend::new(COST_W, 8)).unwrap();
        let area = Rect::new(0, 0, COST_W, 8);
        let row = |b: &ratatui::buffer::Buffer, y: u16| {
            screen_text(b).lines().nth(y as usize).unwrap().to_string()
        };

        t.draw(|f| render_cost(f, area, &v)).unwrap();
        let top = row(t.backend().buffer(), 2);
        let bottom = row(t.backend().buffer(), 6);
        assert!(top.contains("lane") && top.contains("in") && top.contains("out"), "{top:?}");
        // 20 lanes at $0.01 — the total is on screen without scrolling to it
        assert!(bottom.contains("total") && bottom.contains("$0.20"), "{bottom:?}");
        assert!(row(t.backend().buffer(), 3).contains("lane0"));
        // no per-row `tok`: the unit is in the heading
        assert!(!row(t.backend().buffer(), 3).contains("tok"));

        v.cost_scroll.max.set(16);
        v.cost_scroll.by(16);
        t.draw(|f| render_cost(f, area, &v)).unwrap();
        assert_eq!(row(t.backend().buffer(), 2), top, "header scrolled away");
        assert_eq!(row(t.backend().buffer(), 6), bottom, "total scrolled away");
        assert!(row(t.backend().buffer(), 3).contains("lane16"), "body did not scroll");
    }

    #[test]
    fn a_pane_scrolls_until_its_last_line_rests_on_the_bottom_row() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut v = review_stub(0);
        v.focus = ReviewFocus::Summary;
        v.summary = (0..30).map(|i| Line::raw(format!("line{i:02}"))).collect();
        let area = Rect::new(0, 0, 100, 22);
        let mut t = Terminal::new(TestBackend::new(100, 22)).unwrap();
        // which prose lines are on screen, top to bottom — layout-independent,
        // so this keeps testing the scroll and not the box sizes
        let shown = |t: &Terminal<TestBackend>| -> Vec<u32> {
            screen_text(t.backend().buffer())
                .lines()
                .filter_map(|r| r.find("line").map(|i| r[i + 4..i + 6].parse().unwrap()))
                .collect()
        };

        t.draw(|f| render_review_tab(f, area, &v)).unwrap();
        let first = shown(&t);
        assert_eq!(first.first(), Some(&0), "should start at the top: {first:?}");
        assert!(first.len() >= 3, "pane too small to test: {first:?}");

        // hold ↓ down: it must stop with line29 on the bottom row, never scroll
        // the text off into an empty box
        for _ in 0..500 {
            v.summary_scroll.by(1);
            t.draw(|f| render_review_tab(f, area, &v)).unwrap();
        }
        let last = shown(&t);
        assert_eq!(last.last(), Some(&29), "last line must rest on the bottom: {last:?}");
        assert_eq!(last.len(), first.len(), "same screenful, just scrolled");
    }

    /// A red letter in a box title means "press this to get there".
    #[test]
    fn a_red_letter_jumps_straight_to_its_section() {
        let mut r = review_stub(2);
        for (key, want) in [
            ('r', ReviewFocus::Summary),
            ('t', ReviewFocus::Cost),
            ('s', ReviewFocus::Stage),
            ('f', ReviewFocus::Findings),
        ] {
            assert!(matches!(review_key(&mut r, &press(KeyCode::Char(key))), Took::Yes));
            assert!(r.focus == want, "{key} should have jumped");
        }
        // ...and it works from anywhere, not just the findings
        r.focus = ReviewFocus::Cost;
        review_key(&mut r, &press(KeyCode::Char('r')));
        assert!(r.focus == ReviewFocus::Summary);

        // but never while typing an instruction: the letters are text there
        r.focus = ReviewFocus::Findings;
        r.sel = r.note_row();
        review_key(&mut r, &press(KeyCode::Char('t')));
        assert_eq!(r.note.value, "t");
        assert!(r.focus == ReviewFocus::Findings);
        // the buttons hold no letters, so the jumps keep working while the
        // cursor is on them — `→ ↵` is how you pick the second one
        r.focus = ReviewFocus::Findings;
        r.sel = r.action_row();
        r.note.value = "drop it".into();
        assert!(matches!(review_key(&mut r, &press(KeyCode::Char('r'))), Took::Yes));
        assert!(r.focus == ReviewFocus::Summary);
    }

    /// ↓ must not dead-end on the first button; it walks the whole action row.
    #[test]
    fn down_walks_the_buttons_and_comes_back_round_to_the_list() {
        let mut r = review_stub(2);
        for _ in 0..3 {
            review_key(&mut r, &press(KeyCode::Down)); // findings → note → actions
        }
        assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 0));
        review_key(&mut r, &press(KeyCode::Down));
        assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 1), "↓ steps along the row");
        review_key(&mut r, &press(KeyCode::Up));
        assert_eq!((r.sel, r.buttons.sel), (r.action_row(), 0), "↑ steps back");
        review_key(&mut r, &press(KeyCode::Down));
        review_key(&mut r, &press(KeyCode::Down));
        assert_eq!((r.sel, r.buttons.sel), (0, 0), "off the end is the top of the list");
        // and ↑ off the first button still lands on the instruction line
        r.sel = r.action_row();
        review_key(&mut r, &press(KeyCode::Up));
        assert_eq!(r.sel, r.note_row());
    }

    #[test]
    fn review_focus_cycles_both_ways() {
        use ReviewFocus::*;
        assert!(
            Findings.next() == Summary
                && Summary.next() == Cost
                && Cost.next() == Stage
                && Stage.next() == Findings
        );
        // prev is a real inverse, so it survives four
        for f in [Findings, Summary, Cost, Stage] {
            assert!(f.prev().next() == f, "prev/next must be inverse");
            assert!(f.next() != f.prev(), "four variants: next and prev differ");
        }
    }

}

