//! The home screen: the run list, the config/init modal, and the
//! new-feature modal that opens over it.

use crate::review::Review;
use crate::state::{self, State};
use crate::config;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Paragraph, Row, Table,
};
use ratatui::Frame;

use super::*;

// ponytail: claude CLI has no model-list command (probed: it treats "models
// list" as a prompt) — curated list; guvnor.toml accepts any name by hand.
// Aliases resolve to the CLI's latest; versioned ids pin.
pub const MODEL_OPTIONS: [&str; 9] = [
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

// Foreman's lane doctrine translated to one vendor, named by effort:
// highest-judgment model plans, cheaper worker types, strong reviewer —
// never the frontier model in two seats. (name, [planner, worker, reviewer])
pub const MODEL_PRESETS: [(&str, [&str; 3]); 3] = [
    ("max", ["fable", "sonnet", "opus"]),
    ("balanced", ["opus", "sonnet", "opus"]),
    ("budget", ["sonnet", "haiku", "sonnet"]),
];

pub const TITLE_MAX: usize = 72;

/// Which of the home screen's two boxes has the keyboard. The logo between them
/// is decorative — never a focus stop. Tab cycles Runs → new title → new
/// context → Runs.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum HomeFocus {
    Runs,
    New,
}

// (name, test command, tests paths, src paths) — init popup presets.
pub const LANG_PRESETS: [(&str, &str, &[&str], &[&str]); 5] = [
    ("node", "node --test", &["test/"], &["src/"]),
    ("rust", "cargo test", &["tests/"], &["src/"]),
    ("python", "pytest -q", &["tests/"], &["src/"]),
    ("haskell", "cabal test --test-show-details=direct", &["test/"], &["src/", "app/"]),
    ("other", "node --test", &["test/"], &["src/"]),
];

pub struct RunRow {
    pub id: String,
    pub title: String,
    pub status: String,
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

/// One modal for everything guvnor.toml holds. Rows: 0 lang preset · 1 test
/// cmd · 2 tests paths · 3 src paths · 4 model preset · 5/6/7 seats ·
/// 8 claude bin · 9 timeout.
pub struct ConfigView {
    pub row: usize,
    pub preset: usize,  // last stamped LANG_PRESETS entry
    pub mpreset: usize, // last stamped MODEL_PRESETS entry
    pub test: LineInput,
    pub tests: LineInput, // comma-separated
    pub src: LineInput,   // comma-separated
    pub models: [String; 3],
    pub bin: LineInput,
    pub timeout: LineInput,
    pub rework: LineInput,
    /// Open model dropdown for the focused seat: (selection, options).
    pub drop: Option<(usize, Vec<String>)>,
    pub buttons: Buttons,
}

/// Rows 0..10 are settings; the last row is the action row (save/cancel).
pub const CFG_ROWS: usize = 12;

impl ConfigView {
    pub fn from_repo(repo: &std::path::Path) -> Self {
        let cfg = crate::config::Config::load(repo).ok();
        let (test, tests, src) = match &cfg {
            Some(c) => (c.commands.test.clone(), c.paths.tests.join(", "), c.paths.src.join(", ")),
            None => {
                let (_, test, tests, src) = LANG_PRESETS[0];
                (test.to_string(), tests.join(", "), src.join(", "))
            }
        };
        let limits = cfg.as_ref().map(|c| c.limits.clone()).unwrap_or_default();
        let claude = cfg.map(|c| c.claude).unwrap_or_default();
        Self {
            row: 0,
            preset: 0,
            mpreset: 0,
            test: LineInput::with(&test),
            tests: LineInput::with(&tests),
            src: LineInput::with(&src),
            models: [claude.model_planner, claude.model_worker, claude.model_reviewer],
            bin: LineInput::with(&claude.bin),
            timeout: LineInput::with(&limits.lane_timeout_secs.to_string()),
            rework: LineInput::with(&limits.max_rework_rounds.to_string()),
            drop: None,
            buttons: Buttons::new(&["save", "cancel"], YES_NO),
        }
    }

    /// The LineInput behind a text row, if `row` is one.
    pub fn text_input(&mut self, row: usize) -> Option<&mut LineInput> {
        match row {
            1 => Some(&mut self.test),
            2 => Some(&mut self.tests),
            3 => Some(&mut self.src),
            8 => Some(&mut self.bin),
            9 => Some(&mut self.timeout),
            10 => Some(&mut self.rework),
            _ => None,
        }
    }

    pub fn stamp_preset(&mut self, dir: i32) {
        let n = LANG_PRESETS.len() as i32;
        self.preset = (self.preset as i32 + dir).rem_euclid(n) as usize;
        let (_, test, tests, src) = LANG_PRESETS[self.preset];
        self.test = LineInput::with(test);
        self.tests = LineInput::with(&tests.join(", "));
        self.src = LineInput::with(&src.join(", "));
    }

    pub fn stamp_model_preset(&mut self, dir: i32) {
        let n = MODEL_PRESETS.len() as i32;
        self.mpreset = (self.mpreset as i32 + dir).rem_euclid(n) as usize;
        let (_, seats) = MODEL_PRESETS[self.mpreset];
        self.models = seats.map(String::from);
    }

    /// Open the version dropdown for the focused seat (rows 5..=7): current
    /// value first when it's not in the curated list, so nothing is destroyed.
    pub fn open_drop(&mut self) {
        let cur = &self.models[self.row - 5];
        let mut options: Vec<String> = Vec::new();
        if !MODEL_OPTIONS.contains(&cur.as_str()) {
            options.push(cur.clone());
        }
        options.extend(MODEL_OPTIONS.iter().map(|m| m.to_string()));
        let sel = options.iter().position(|m| m == cur).unwrap_or(0);
        self.drop = Some((sel, options));
    }
}

/// Cycle a model seat through the curated list; unknown names enter at the
/// start so hand-edited full model names are never destroyed silently.
pub fn cycle_model(cur: &str, dir: i32) -> String {
    let n = MODEL_OPTIONS.len() as i32;
    let next = match MODEL_OPTIONS.iter().position(|m| *m == cur) {
        Some(i) => (i as i32 + dir).rem_euclid(n) as usize,
        None => if dir > 0 { 0 } else { (n - 1) as usize },
    };
    MODEL_OPTIONS[next].into()
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
                    status: st.status.to_string(),
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

    /// Validate + write the config modal to guvnor.toml (creates .guvnor/ on
    /// first save — this IS the in-app init).
    pub fn save_config(&mut self) {
        let Some(cv) = &self.config else { return };
        let timeout: u64 = match cv.timeout.value.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.toast = toast("timeout must be a number of seconds");
                return;
            }
        };
        let rework: u64 = match cv.rework.value.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.toast = toast("rework rounds must be a number (0 disables)");
                return;
            }
        };
        let split = |s: &str| -> Vec<String> {
            s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
        };
        let (tests, src) = (split(&cv.tests.value), split(&cv.src.value));
        let settings = config::Settings {
            test: cv.test.value.trim().to_string(),
            tests,
            src,
            bin: cv.bin.value.trim().to_string(),
            models: cv.models.clone(),
            timeout_secs: timeout,
            max_rework_rounds: rework,
        };
        match config::save_settings(&self.repo, &settings) {
            Ok(p) => {
                self.toast = toast(format!("saved {}", p.display()));
                self.config = None;
                self.reload_runs();
            }
            Err(e) => self.toast = toast(format!("{e:#}")),
        }
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
            // line up in a column instead of leaving a ragged gap.
            let status_w = vis
                .iter()
                .map(|i| status_badge(&self.runs[*i].status).content.chars().count())
                .max()
                .unwrap_or(0);
            let rows: Vec<Row> = vis
                .iter()
                .enumerate()
                .map(|(pos, i)| {
                    let r = &self.runs[*i];
                    let selected = Some(pos) == sel;
                    let mut status = if running.as_deref() == Some(&r.id) {
                        Span::styled(format!("{} running", spin_frame()), Style::new().fg(Color::Cyan))
                    } else {
                        pad_badge(status_badge(&r.status), status_w)
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

    pub fn render_runs_popups(&self, f: &mut Frame, area: Rect) {
        if let Some(cv) = &self.config {
            let [pc] = Layout::horizontal([Constraint::Length(68.min(area.width))])
                .flex(Flex::Center)
                .areas(area);
            // the settings breathe: a lead blank, then every row followed by a
            // blank — 11 rows → 22 lines — and one spare line off the actions
            // box, so nothing ever needs to scroll on a terminal this tall.
            // + the boxed actions (3) + borders (2).
            let body = (CFG_ROWS as u16 - 1) * 2 + 1;
            let h = (body + 5).min(area.height);
            let [popup] = Layout::vertical([Constraint::Length(h)]).flex(Flex::Center).areas(pc);
            f.render_widget(Clear, popup);
            let title = if self.initialized {
                " config — .guvnor/guvnor.toml "
            } else {
                " configure + initialise this repo "
            };
            let block = modal(
                title.trim(),
                &[("←/→", "cycle"), ("↵", "pick / act"), ("esc", "cancel")],
            );
            let outer_inner = block.inner(popup);
            f.render_widget(block, popup);
            // the actions keep their 3 rows on any terminal; the settings take
            // what's left and scroll to keep the selected row in view.
            let [inner, cbtn_a] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(outer_inner);
            let cv_focused = cv.row == CFG_ROWS - 1;
            cv.buttons.render(f, cbtn_a, cv_focused);
            let sel_style = Style::new().bg(Color::White).fg(Color::Black).bold();
            let dim = Style::new().fg(Color::DarkGray);
            let labels = [
                "language preset",
                "test command",
                "tests paths",
                "src paths",
                "model preset",
                "model planner",
                "model worker",
                "model reviewer",
                "claude bin",
                "timeout (s)",
                "rework rounds",
            ];
            let mut lines: Vec<Line> = vec![Line::raw("")]; // lead blank, like every box
            for (i, label) in labels.iter().enumerate() {
                let sel = i == cv.row;
                if i > 0 {
                    lines.push(Line::raw("")); // one blank between every option
                }
                // ponytail: no h-scroll inside the modal; toml values this long
                // deserve $EDITOR anyway
                let value = match i {
                    0 => format!("◀ {} ▶", LANG_PRESETS[cv.preset].0),
                    1 => cv.test.value.clone(),
                    2 => cv.tests.value.clone(),
                    3 => cv.src.value.clone(),
                    4 => format!("◀ {} ▶", MODEL_PRESETS[cv.mpreset].0),
                    5..=7 => format!("{} ▾", cv.models[i - 5]),
                    8 => cv.bin.value.clone(),
                    9 => cv.timeout.value.clone(),
                    _ => cv.rework.value.clone(),
                };
                lines.push(Line::from(vec![
                    Span::raw(if sel { " ▶ " } else { "   " }),
                    Span::styled(format!("{label:<15} "), dim),
                    Span::styled(value, if sel { sel_style } else { Style::new() }),
                ]));
            }
            // row i sits on line 1 + 2i; scroll only if the box is too short.
            // The action row (the last) has no line of its own — clamping to the
            // last setting is what stops the list lurching when you land on it.
            let y = 1 + cv.row.min(labels.len() - 1) as u16 * 2;
            let off = y.saturating_sub(inner.height.saturating_sub(1));
            f.render_widget(Paragraph::new(lines).scroll((off, 0)), inner);
            let input = match cv.row {
                1 => Some(&cv.test),
                2 => Some(&cv.tests),
                3 => Some(&cv.src),
                8 => Some(&cv.bin),
                9 => Some(&cv.timeout),
                10 => Some(&cv.rework),
                _ => None,
            };
            if let Some(inp) = input {
                f.set_cursor_position(Position::new(
                    inner.x + 19 + inp.cursor as u16,
                    inner.y + y - off,
                ));
            }
            // model version dropdown, overlaying the seat rows
            if let Some((sel, options)) = &cv.drop {
                let seat = ["planner", "worker", "reviewer"][cv.row - 5];
                let w = 30.min(area.width);
                let dh = (options.len() as u16 + 2).min(area.height);
                let [dc] = Layout::horizontal([Constraint::Length(w)]).flex(Flex::Center).areas(area);
                let [dpopup] = Layout::vertical([Constraint::Length(dh)]).flex(Flex::Center).areas(dc);
                f.render_widget(Clear, dpopup);
                let dblock =
                    modal(&format!("model {seat}"), &[("↵", "choose"), ("esc", "back")]);
                let dinner = dblock.inner(dpopup);
                f.render_widget(dblock, dpopup);
                let current = &cv.models[cv.row - 5];
                let dlines: Vec<Line> = options
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let mark = if m == current { "✓ " } else { "  " };
                        Line::styled(
                            format!(" {mark}{m}"),
                            if i == *sel { sel_style } else { Style::new() },
                        )
                    })
                    .collect();
                f.render_widget(
                    Paragraph::new(dlines).scroll((sel.saturating_sub(dinner.height.saturating_sub(1) as usize) as u16, 0)),
                    dinner,
                );
            }
            return; // the config modal owns the screen
        }
        if let Some((id, title, buttons)) = &self.confirm_delete {
            // Binning a staged run leaves its patch applied with no `unstage` to
            // undo it — the one delete that costs you something, so it says so.
            let warn = " it's staged — the patch stays in your tree; git restore undoes it";
            let staged = self.runs.iter().any(|r| r.id == *id && r.status == "staged");
            let want = (title.chars().count() as u16 + 16)
                .max(if staged { warn.chars().count() as u16 + 2 } else { 0 });
            let w = want.clamp(38, area.width);
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
    let tw = title_a.width.saturating_sub(2) as usize;
    let txoff = v.title.cursor.saturating_sub(tw.saturating_sub(1));
    f.render_widget(
        Paragraph::new(v.title.value.as_str()).scroll((0, txoff as u16)).block(title_block),
        title_a,
    );
    let cinner = context_block.inner(context_a);
    f.render_widget(context_block, context_a);
    render_textarea(f, cinner, &v.context, context_on);
    if title_on {
        f.set_cursor_position(Position::new(
            title_a.x + 1 + (v.title.cursor - txoff) as u16,
            title_a.y + 1,
        ));
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
        let screen: String = {
            let buf = t.backend().buffer();
            (0..50)
                .flat_map(|y| (0..120).map(move |x| (x, y)))
                .map(|(x, y)| buf[(x, y)].symbol().to_string())
                .collect()
        };
        assert!(screen.contains("feature title"), "the title field is always drawn: {screen:?}");
        assert!(screen.contains("context for the planner"), "and the context field");
        // the responsive art tiers (full / letters-only / none) must not panic
        // the 50/50 split on a cramped screen
        for (w, h) in [(120u16, 50u16), (100, 30), (40, 14)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| app.render_runs(f, Rect::new(0, 0, w, h))).unwrap();
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
    /// the rest of the row. The old `.reversed()` flipped the chips to black.
    #[test]
    fn selecting_a_row_keeps_badge_colours_and_bars_the_rest() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let row = |status: &str| RunRow {
            id: format!("id-{status}"),
            title: "a feature".into(),
            status: status.into(),
            verdict: "APPROVED".into(),
            cost: "$1.00".into(),
            gates: gates_line(&crate::state::Gates::default()),
        };
        let mut app = App::for_test();
        app.runs = vec![row("reviewed"), row("committed")]; // cyan, green
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
        let row = |status: &str| RunRow {
            id: format!("id-{status}"),
            title: status.into(),
            status: status.into(),
            verdict: String::new(),
            cost: String::new(),
            gates: gates_line(&crate::state::Gates::default()),
        };
        for status in ["planned", "reviewed", "staged", "failed:vacuous_tests"] {
            let mut app = App::for_test();
            app.runs = vec![row(status)];
            app.table.select(Some(0));
            app.handle_key(&press(KeyCode::Char('d')));
            assert!(app.confirm_delete.is_some(), "{status} must be deletable");
        }
        let mut app = App::for_test();
        app.runs = vec![row("committed")];
        app.table.select(Some(0));
        app.handle_key(&press(KeyCode::Char('d')));
        assert!(app.confirm_delete.is_none(), "a committed run is the record — no delete");
        assert!(app.toast.is_some(), "and it says why");
    }

    /// The config modal is a form, not a list: one blank line between every
    /// option, and the ▶ marker (plus the text cursor) still lands on the row
    /// it names once those blanks shift everything down.
    #[test]
    fn config_options_are_blank_separated() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        // where each row lands, drawn with `row` selected
        let draw = |row: usize| -> (Vec<String>, u16) {
            let mut app = App::for_test();
            app.config = Some(ConfigView::from_repo(&app.repo));
            app.config.as_mut().unwrap().row = row;
            let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
            t.draw(|f| app.render_runs_popups(f, Rect::new(0, 0, 120, 40))).unwrap();
            let buf = t.backend().buffer().clone();
            let lines: Vec<String> = (0..40)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let at = lines.iter().position(|l| l.contains("language preset")).unwrap() as u16;
            (lines, at)
        };
        let (lines, a) = draw(1); // "test command"
        let b = lines.iter().position(|l| l.contains("test command")).unwrap() as u16;
        assert_eq!(b - a, 2, "one blank line between every option");
        // the ▶ marker opens the row, inside the modal's left border (the row 0
        // value is `◀ node ▶`, so "contains" would lie here)
        let marked = |l: &str| l.split('│').nth(1).is_some_and(|s| s.trim_start().starts_with('▶'));
        assert!(marked(&lines[b as usize]), "the marker follows the selected row");
        assert!(!marked(&lines[a as usize]), "and only that row");
        // stepping onto the action row must not shunt the list: the modal is
        // tall enough for every option, so nothing scrolls.
        assert_eq!(draw(CFG_ROWS - 1).1, a, "the list must not jump on the buttons row");
    }

    #[test]
    fn cycle_model_wraps_and_handles_custom() {
        assert_eq!(cycle_model("opus", 1), "sonnet");
        assert_eq!(cycle_model("opus", -1), MODEL_OPTIONS[MODEL_OPTIONS.len() - 1]);
        assert_eq!(cycle_model("a-hand-edited-model", 1), "opus");
    }

    #[test]
    fn open_drop_keeps_custom_model_first() {
        let mut cv = ConfigView {
            row: 5,
            preset: 0,
            mpreset: 0,
            test: LineInput::default(),
            tests: LineInput::default(),
            src: LineInput::default(),
            models: ["my-custom-model".into(), "sonnet".into(), "opus".into()],
            bin: LineInput::default(),
            timeout: LineInput::default(),
            rework: LineInput::default(),
            drop: None,
            buttons: Buttons::new(&["save", "cancel"], YES_NO),
        };
        cv.open_drop();
        let (sel, options) = cv.drop.as_ref().unwrap();
        assert_eq!(options[0], "my-custom-model");
        assert_eq!(*sel, 0);
        assert_eq!(options.len(), MODEL_OPTIONS.len() + 1);
        // known model: no duplicate entry, selection lands on it
        cv.row = 6;
        cv.open_drop();
        let (sel, options) = cv.drop.as_ref().unwrap();
        assert_eq!(options.len(), MODEL_OPTIONS.len());
        assert_eq!(options[*sel], "sonnet");
    }

}

