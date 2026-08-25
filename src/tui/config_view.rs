//! The config/init modal: every setting guvnor.toml holds, plus the model
//! presets and the version dropdown.

use crate::config;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use super::*;

// The claude CLI has no model-list command (probed: it treats "models
// list" as a prompt), so this is a curated list; guvnor.toml accepts any
// name by hand regardless. Aliases resolve to the CLI's latest; versioned
// ids pin.
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

// One vendor, three lanes named by effort: highest-judgment model plans,
// cheaper worker types, strong reviewer — never the frontier model in two
// seats. (name, [planner, worker, reviewer])
pub const MODEL_PRESETS: [(&str, [&str; 3]); 3] = [
    ("max", ["fable", "sonnet", "opus"]),
    ("balanced", ["opus", "sonnet", "opus"]),
    ("budget", ["sonnet", "haiku", "sonnet"]),
];

// (name, test command, tests paths, src paths) — init popup presets.
pub const LANG_PRESETS: [(&str, &str, &[&str], &[&str]); 5] = [
    ("node", "node --test", &["test/"], &["src/"]),
    ("rust", "cargo test", &["tests/"], &["src/"]),
    ("python", "pytest -q", &["tests/"], &["src/"]),
    ("haskell", "cabal test --test-show-details=direct", &["test/"], &["src/", "app/"]),
    ("other", "node --test", &["test/"], &["src/"]),
];

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

    /// The LineInput behind a text row, if `row` is one — the single row→field
    /// table: the key handler types through it and the render side places the
    /// cursor with it, so the two cannot drift.
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

    /// The config modal + its model-version dropdown, over the home screen.
    pub fn render_config_modal(&mut self, f: &mut Frame, area: Rect) {
        let Some(cv) = &mut self.config else { return };
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
            // No h-scroll inside the modal, by design: a toml value too long for this
            // box to show is a value that belongs in $EDITOR, which is always one key
            // away.
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
        // `text_input` is the one row→field table, shared with the key
        // handler — the cursor cannot sit on a row the keys don't edit.
        if let Some(inp) = cv.text_input(cv.row) {
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
    }

}

#[cfg(test)]
mod tests {
    use super::*;

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
            let lines: Vec<String> =
                screen_text(t.backend().buffer()).lines().map(String::from).collect();
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
