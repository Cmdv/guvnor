//! The home screen: the run list, the config/init modal, and the
//! new-feature modal that opens over it.

use crate::review::Review;
use crate::state::{self, State};
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Paragraph, Row, Table,
};
use ratatui::Frame;

use super::*;

pub const TITLE_MAX: usize = 72;

/// Which of the home screen's two boxes has the keyboard. The logo between them
/// is decorative — never a focus stop. Tab cycles Runs → new title → new
/// context → Runs.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum HomeFocus {
    Runs,
    New,
}

pub struct RunRow {
    pub id: String,
    pub title: String,
    /// Typed, not pre-stringified: comparisons stay on the enum; the badge
    /// renders from `to_string()` at draw time.
    pub status: state::Status,
    pub verdict: String,
    pub cost: String, // total spend from events.ndjson, "" when none
    pub gates: Line<'static>,
}

pub struct NewView {
    pub title: LineInput,
    pub context: TextArea,
    /// 0 title · 1 context. Tab cycles. No action row: ↵ submits and esc
    /// cancels, which is two fewer things on screen than a row saying so.
    pub focus: usize,
}

impl Default for NewView {
    fn default() -> Self {
        Self {
            title: LineInput { max: TITLE_MAX, ..Default::default() },
            context: TextArea::default(),
            focus: 0,
        }
    }
}

impl App {

    // ---- data loading ----------------------------------------------------

    pub fn reload_runs(&mut self) {
        let mut rows = Vec::new();
        if let Ok(entries) = std::fs::read_dir(state::runs_root(&self.repo)) {
            for e in entries.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let Ok(st) = State::load(&e.path()) else { continue };
                let verdict = std::fs::read_to_string(e.path().join("review.json"))
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Review>(&raw).ok())
                    .map(|r| r.verdict.verdict.to_string())
                    .unwrap_or_default();
                let cost = crate::casefile::total_cost(&e.path());
                rows.push(RunRow {
                    gates: gates_line(&st.gates),
                    id: st.id,
                    title: st.title,
                    status: st.status,
                    verdict,
                    cost: if cost > 0.0 { format!("${cost:.2}") } else { String::new() },
                });
            }
        }
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        self.runs = rows;
        self.initialized = self.repo.join(".guvnor/guvnor.toml").is_file();
        self.cfg_models = crate::config::Config::load(&self.repo)
            .ok()
            .map(|c| [c.claude.model_planner, c.claude.model_worker, c.claude.model_reviewer]);
        self.clamp_selection();
    }

    /// Indexes into self.runs that pass the title filter.
    pub fn visible_idx(&self) -> Vec<usize> {
        let q = self.filter.value.to_lowercase();
        self.runs
            .iter()
            .enumerate()
            .filter(|(_, r)| q.is_empty() || r.title.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_run(&self) -> Option<&RunRow> {
        let vis = self.visible_idx();
        self.table.selected().and_then(|s| vis.get(s)).map(|i| &self.runs[*i])
    }

    pub fn clamp_selection(&mut self) {
        let len = self.visible_idx().len();
        let sel = self.table.selected().unwrap_or(0);
        self.table.select(if len == 0 { None } else { Some(sel.min(len - 1)) });
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.visible_idx().len();
        if len == 0 {
            return;
        }
        let sel = self.table.selected().unwrap_or(0) as i32 + delta;
        self.table.select(Some(sel.clamp(0, len as i32 - 1) as usize));
    }

    pub fn render_runs(&mut self, f: &mut Frame, area: Rect) {
        // ---- art band: a 50/50 row — the logo boxed in the left half, the
        // always-present new-feature panel filling the right. Whole art pieces
        // only (mask+lettering → lettering → none).
        let mask = GUV_MASK.trim_matches('\n');
        let mask_h = mask.lines().count() as u16;
        let mask_w = mask.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let letter_w = GUV_LETTER.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let letter_h = GUV_LETTER.lines().count() as u16 + 1;
        let art_w = letter_w.max(mask_w);
        // content rows, then the box's own: a blank lead line + two borders.
        let full_c = letter_h + mask_h + 1;
        let extra = 3;
        let art_h = if area.height >= full_c + extra + 10 && area.width >= art_w + 4 {
            full_c + extra
        } else if area.height >= letter_h + extra + 10 && area.width >= art_w + 4 {
            letter_h + extra
        } else {
            0
        };
        let [art_a, runs_a, cfg_a] = Layout::vertical([
            Constraint::Length(art_h),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .areas(area);
        if art_h > 0 {
            // two 50/50 columns: logo left, new-feature panel right. The panel
            // fills its column and the whole row height (dictated by the logo).
            let [logo_col, new_col] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(art_a);
            // the art hugs its half and centres there; it's decorative — no
            // border, no focus, just a blank lead line for breathing room.
            let [logo_a] = Layout::horizontal([Constraint::Length(art_w + 4)])
                .flex(Flex::Center)
                .areas(logo_col);
            let [_, inner] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(logo_a);
            // each piece in its own Flex-centered exact-width column: shapes stay intact
            let [letter_a, mask_a] =
                Layout::vertical([Constraint::Length(letter_h), Constraint::Min(0)]).areas(inner);
            let [lc] = Layout::horizontal([Constraint::Length(letter_w)])
                .flex(Flex::Center)
                .areas(letter_a);
            f.render_widget(
                Paragraph::new(art_lines(GUV_LETTER, Style::new().fg(ART_WHITE).bold())),
                lc,
            );
            if art_h == full_c + extra {
                let [mc] = Layout::horizontal([Constraint::Length(mask_w)])
                    .flex(Flex::Center)
                    .areas(mask_a);
                f.render_widget(Paragraph::new(art_lines(mask, Style::new().fg(ART_WHITE))), mc);
            }
            render_new_box(f, new_col, &self.new, self.focus == HomeFocus::New);
        }
        // ---- runs section (filter-aware; actions on the border, btop-style)
        let mut block = boxed("runs", Style::new().bold());
        if self.focus == HomeFocus::Runs {
            let white = Style::new().fg(Color::White);
            block = block.border_style(white).title_style(white);
        }
        if self.filtering || !self.filter.value.is_empty() {
            let cursor = if self.filtering { "▏" } else { "" };
            block = block.title(Line::from(vec![
                Span::styled(" filter: ", Style::new().fg(Color::DarkGray)),
                Span::styled(format!("{}{cursor} ", self.filter.value), Style::new().fg(Color::Yellow)),
            ]));
        }
        if !self.filtering {
            // the actions hang off the bottom border, like every other box's
            // hints — the top border is for the title and the filter.
            block = block.title_bottom(hint_line(&[
                ("↵", "open"),
                ("f", "filter"),
                ("d", "delete"),
            ]));
        }
        let vis = self.visible_idx();
        if vis.is_empty() {
            let msg = if self.runs.is_empty() {
                "\n  no runs yet — press n to plan a feature"
            } else {
                "\n  no runs match the filter — esc clears it"
            };
            f.render_widget(Paragraph::new(msg).block(block), runs_a);
        } else {
            let running = self.job.as_ref().and_then(|j| j.run_id.clone());
            let sel = self.table.selected();
            // On the selected row: a chip (a span that carries a background)
            // keeps its colour but gets light-grey letters, so it reads as
            // selected too; bare text (the verdict, the gate dividers, the
            // muted "not yet" chips) takes the row's black. Off the bar, spans
            // are as-is.
            let recolour = |s: Style| {
                if s.bg.is_some() {
                    s.fg(Color::Gray)
                } else {
                    s.fg(Color::Black)
                }
            };
            // pad every status chip to the widest shown, so the coloured blocks
            // line up in a column instead of leaving a ragged gap. One badge per
            // row, built once: the width pass and the row pass read the same spans.
            let badges: Vec<Span> =
                vis.iter().map(|i| status_badge(&self.runs[*i].status)).collect();
            let status_w = badges.iter().map(|b| b.content.chars().count()).max().unwrap_or(0);
            let rows: Vec<Row> = vis
                .iter()
                .enumerate()
                .map(|(pos, i)| {
                    let r = &self.runs[*i];
                    let selected = Some(pos) == sel;
                    let mut status = if running.as_deref() == Some(&r.id) {
                        Span::styled(format!("{} running", spin_frame()), Style::new().fg(Color::Cyan))
                    } else {
                        pad_badge(badges[pos].clone(), status_w)
                    };
                    let verdict = if selected {
                        Span::raw(r.verdict.clone())
                    } else {
                        verdict_span(&r.verdict)
                    };
                    let mut gates = r.gates.clone();
                    if selected {
                        // the chip keeps its background; only its letters grey out
                        if status.style.bg.is_some() {
                            status.style = status.style.fg(Color::Gray);
                        }
                        for span in &mut gates.spans {
                            span.style = recolour(span.style);
                        }
                    }
                    let row = Row::new(vec![
                        Cell::from(r.title.clone()),
                        Cell::from(status),
                        Cell::from(verdict),
                        Cell::from(r.cost.clone()),
                        Cell::from(gates),
                    ]);
                    // The selected row wears the bar as its BASE style, not a
                    // highlight over the top: the badges' own backgrounds patch
                    // over it, so their colours (cyan, green, …) survive.
                    if selected {
                        row.style(Style::new().bg(ART_WHITE).fg(Color::Black))
                    } else {
                        row
                    }
                })
                .collect();
            let table = Table::new(
                rows,
                [
                    Constraint::Min(20),
                    Constraint::Length(18),
                    Constraint::Length(9),
                    Constraint::Length(7),
                    Constraint::Length(27),
                ],
            )
            .header(Row::new(["title", "status", "verdict", "cost", "gates"]).style(Style::new().bold().fg(Color::DarkGray)))
            .block(block);
            f.render_stateful_widget(table, runs_a, &mut self.table);
        }
        // ---- config section
        let dim = Style::new().fg(Color::DarkGray);
        let red = Style::new().fg(Color::Red).bold();
        let cfg_lines = if !self.initialized {
            vec![
                Line::from(vec![
                    Span::raw(" no .guvnor/guvnor.toml — press "),
                    Span::styled("c", red),
                    Span::raw(" to configure + initialise this repo"),
                ]),
                Line::styled(" guvnor needs a test command and tests/src paths to run lanes", dim),
            ]
        } else if let Some([p, w, r]) = &self.cfg_models {
            vec![
                Line::from(vec![
                    Span::styled(" planner ", dim),
                    Span::styled(p.clone(), Style::new().bold()),
                    Span::styled("   worker ", dim),
                    Span::styled(w.clone(), Style::new().bold()),
                    Span::styled("   reviewer ", dim),
                    Span::styled(r.clone(), Style::new().bold()),
                ]),
                Line::styled(" everything else lives in .guvnor/guvnor.toml", dim),
            ]
        } else {
            vec![Line::raw(" guvnor.toml unreadable — fix it in your editor")]
        };
        f.render_widget(
            Paragraph::new(cfg_lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(MODAL_BORDER))
                    .title_style(Style::new().fg(MODAL_BORDER))
                    .title(box_title("config", "c", Style::new().bold(), false)),
            ),
            cfg_a,
        );
    }

    pub fn render_runs_popups(&mut self, f: &mut Frame, area: Rect) {
        if self.config.is_some() {
            self.render_config_modal(f, area);
            return; // the config modal owns the screen
        }
        if let Some((id, title, buttons)) = &self.confirm_delete {
            // Binning a staged run leaves its patch applied with no `unstage` to
            // undo it — the one delete that costs you something, so it says so.
            let warn = " it's staged — the patch stays in your tree; git restore undoes it";
            let staged = self.runs.iter().any(|r| r.id == *id && r.status == state::Status::Staged);
            let want = (title.chars().count() as u16 + 16)
                .max(if staged { warn.chars().count() as u16 + 2 } else { 0 });
            // A terminal narrower than the 38-column floor gives up the floor,
            // not the frame: `clamp` asserts min <= max and would panic.
            let w = want.clamp(38.min(area.width), area.width);
            let [pc] = Layout::horizontal([Constraint::Length(w)]).flex(Flex::Center).areas(area);
            let [popup] = Layout::vertical([Constraint::Length(8)]).flex(Flex::Center).areas(pc);
            // one title only — ratatui stacks `.title()` calls, and modal()
            // already hung it. The danger is the red `delete` button, per the
            // app's one device: red means "press this".
            let block = modal("delete run", &[("←/→", "choose"), ("↵", "confirm")]);
            let inner = block.inner(popup);
            f.render_widget(Clear, popup);
            f.render_widget(block, popup);
            let [msg_a, btn_a] =
                Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).areas(inner);
            // leading blank: every box in the app opens with one
            let mut msg = vec![Line::raw(""), Line::raw(format!(" delete '{title}'?"))];
            if staged {
                msg.push(Line::styled(warn, Style::new().fg(Color::Yellow)));
            }
            f.render_widget(Paragraph::new(msg), msg_a);
            buttons.render(f, btn_a, true);
        }
    }

}

/// Draw the always-present new-feature panel into `slot`: a title input over a
/// multiline context box. All the chrome (borders + labels) is the same calm
/// grey; focus is the one thing marked brighter — a white border — so the
/// active box is the only thing that stands out. The inner fields only light
/// up, and only carry the cursor, when the panel itself is focused.
fn render_new_box(f: &mut Frame, slot: Rect, v: &NewView, focused: bool) {
    let grey = Style::new().fg(MODAL_BORDER);
    // border and its hanging brackets move together: white when the box holds
    // focus, grey otherwise (title_style is what carries the brackets).
    let border = |on: bool| Style::new().fg(if on { Color::White } else { MODAL_BORDER });
    // the red `n` is the key that jumps here, per the app's one device.
    let block = boxed("", Style::new())
        .title(box_title("new feature", "n", grey.bold(), false))
        .border_style(border(focused))
        .title_style(border(focused))
        .title_bottom(hint_line(&[
            ("tab", "field"),
            ("⇧↵", "newline"),
            ("↵", "plan it"),
            ("esc", "cancel"),
        ]));
    let inner = block.inner(slot);
    f.render_widget(block, slot);
    let [title_a, context_a] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(inner);
    let (title_on, context_on) = (focused && v.focus == 0, focused && v.focus == 1);
    let title_block = boxed(&format!("feature title (max {TITLE_MAX} chars)"), grey)
        .border_style(border(title_on))
        .title_style(border(title_on));
    let context_block = boxed("context for the planner (optional, multiline)", grey)
        .border_style(border(context_on))
        .title_style(border(context_on));
    // horizontal scroll keeps the cursor on-screen in both inputs
    let (txoff, cx) = hscroll(v.title.cursor, title_a.width.saturating_sub(2) as usize);
    f.render_widget(
        Paragraph::new(v.title.value.as_str()).scroll((0, txoff)).block(title_block),
        title_a,
    );
    let cinner = context_block.inner(context_a);
    f.render_widget(context_block, context_a);
    render_textarea(f, cinner, &v.context, context_on);
    if title_on {
        f.set_cursor_position(Position::new(title_a.x + 1 + cx, title_a.y + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// The new-feature box has no action row: ↵ plans it from either field, and
    /// the only way to get a newline into the context is ⇧↵. It's the focused
    /// box on the home screen, not a separate screen.
    #[test]
    fn enter_plans_it_and_shift_enter_types_a_newline() {
        let dir = std::env::temp_dir().join(format!("guvnor-newkeys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(dir.clone(), false);
        app.config = None; // an uninitialised repo greets you with the config modal
        app.focus = HomeFocus::New; // tab off the runs list onto the panel
        let key = |code, m| KeyEvent::new(code, m);
        let typed = |app: &mut App, s: &str| {
            for c in s.chars() {
                app.handle_key(&key(KeyCode::Char(c), KeyModifiers::NONE));
            }
        };

        // no title, no job — and it says so instead of planning nothing
        assert!(app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)).is_none());
        assert!(app.toast.is_some());

        typed(&mut app, "add stats");
        app.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE));
        typed(&mut app, "one");
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        typed(&mut app, "two");
        match app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)) {
            Some(Go::Plan(title, context)) => {
                assert_eq!(title, "add stats");
                assert_eq!(context, "one\ntwo", "⇧↵ is the newline, ↵ is the submit");
            }
            other => panic!("↵ should have planned it, got {}", other.is_none()),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The new-feature panel is a permanent box on the home screen — no key
    /// press, no separate screen. Its two inner fields are always drawn.
    #[test]
    fn the_panel_is_always_on_the_home_screen() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = App::for_test();
        let mut t = Terminal::new(TestBackend::new(120, 50)).unwrap();
        t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 50))).unwrap();
        let screen = screen_text(t.backend().buffer());
        assert!(screen.contains("feature title"), "the title field is always drawn: {screen:?}");
        assert!(screen.contains("context for the planner"), "and the context field");
        // the responsive art tiers (full / letters-only / none) must not panic
        // the 50/50 split on a cramped screen
        for (w, h) in [(120u16, 50u16), (100, 30), (40, 14)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| app.render_runs(f, Rect::new(0, 0, w, h))).unwrap();
        }
    }

    /// A panic inside `draw` escapes before the terminal is handed back, so a
    /// narrow window is not a cosmetic problem: it leaves the shell in raw mode.
    /// The delete popup wants 38 columns and has to cope with fewer.
    #[test]
    fn the_delete_popup_survives_a_terminal_narrower_than_it_wants() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = App::for_test();
        app.runs = vec![RunRow {
            id: "id-1".into(),
            title: "a title long enough to want more room than this".into(),
            status: state::Status::Staged,
            verdict: String::new(),
            cost: String::new(),
            gates: gates_line(&crate::state::Gates::default()),
        }];
        app.table.select(Some(0));
        app.handle_key(&press(KeyCode::Char('d')));
        assert!(app.confirm_delete.is_some());
        // the whole dispatch, so the popup gets the inner rect it gets for real
        for w in [10u16, 20, 39, 40, 120] {
            let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
            t.draw(|f| app.render(f)).unwrap();
        }
    }

    /// Tab walks the keyboard Runs → title → context → Runs; esc from the panel
    /// hands it straight back. The runs list holds focus at startup.
    #[test]
    fn tab_cycles_focus_between_runs_and_the_panel() {
        let mut app = App::for_test();
        assert!(app.focus == HomeFocus::Runs, "home starts on the runs list");
        app.handle_key(&press(KeyCode::Tab));
        assert!(
            app.focus == HomeFocus::New && app.new.focus == 0,
            "tab enters the panel at the title"
        );
        app.handle_key(&press(KeyCode::Tab));
        assert!(app.focus == HomeFocus::New && app.new.focus == 1, "then the context");
        app.handle_key(&press(KeyCode::Tab));
        assert!(app.focus == HomeFocus::Runs, "then back to the runs list");
        app.handle_key(&press(KeyCode::Tab)); // onto the panel again
        app.handle_key(&press(KeyCode::Esc));
        assert!(app.focus == HomeFocus::Runs, "esc hands focus back to the list");
    }

    /// Selecting a row must leave the coloured chips alone — not just green, but
    /// every badge colour (the cyan `reviewed` here) — and lay a plain bar over
    /// the rest of the row.
    #[test]
    fn selecting_a_row_keeps_badge_colours_and_bars_the_rest() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let row = |status: state::Status| RunRow {
            id: format!("id-{status}"),
            title: "a feature".into(),
            status,
            verdict: "APPROVED".into(),
            cost: "$1.00".into(),
            gates: gates_line(&crate::state::Gates::default()),
        };
        let mut app = App::for_test();
        app.runs = vec![row(state::Status::Reviewed), row(state::Status::Committed)]; // cyan, green
        app.table.select(Some(0)); // the cyan `reviewed` row is the selected one
        let mut t = Terminal::new(TestBackend::new(120, 20)).unwrap();
        t.draw(|f| app.render_runs(f, Rect::new(0, 0, 120, 20))).unwrap();
        let buf = t.backend().buffer().clone();
        let cell = |want: Color| {
            (0..20)
                .flat_map(|y| (0..120).map(move |x| (x, y)))
                .map(|(x, y)| buf[(x, y)].clone())
                .find(|c| c.style().bg == Some(want))
        };
        // the cyan reviewed chip is on the selected row: it keeps its cyan
        // background but its letters go light grey to read as selected.
        let cyan = cell(Color::Cyan).expect("the reviewed chip keeps its cyan background");
        assert_eq!(cyan.style().fg, Some(Color::Gray), "selected chip gets grey letters");
        assert!(cell(Color::Green).is_some(), "other badges keep their colour too");
        assert!(cell(ART_WHITE).is_some(), "the selected row wears the plain bar");
    }

    /// `d` bins any run you haven't landed — planned, failed, staged alike.
    /// The one exception is a committed run: its evidence is the record behind
    /// a commit that already exists.
    #[test]
    fn d_deletes_anything_but_a_committed_run() {
        use state::Status;
        let row = |status: Status| RunRow {
            id: format!("id-{status}"),
            title: status.to_string(),
            status,
            verdict: String::new(),
            cost: String::new(),
            gates: gates_line(&crate::state::Gates::default()),
        };
        for status in
            [Status::Planned, Status::Reviewed, Status::Staged, Status::Failed("vacuous_tests".into())]
        {
            let mut app = App::for_test();
            app.runs = vec![row(status.clone())];
            app.table.select(Some(0));
            app.handle_key(&press(KeyCode::Char('d')));
            assert!(app.confirm_delete.is_some(), "{status} must be deletable");
        }
        let mut app = App::for_test();
        app.runs = vec![row(Status::Committed)];
        app.table.select(Some(0));
        app.handle_key(&press(KeyCode::Char('d')));
        assert!(app.confirm_delete.is_none(), "a committed run is the record — no delete");
        assert!(app.toast.is_some(), "and it says why");
    }

}

