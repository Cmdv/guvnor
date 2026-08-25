//! Keyboard-driven TUI: the whole gate loop from `guvnor` with no args.
//! Same engine as the CLI verbs, consumed over the same mpsc events.
//!
//! One module per screen, plus the primitives they share. `App` owns the
//! state and the event loop; `keys` owns what every keystroke means.

use crate::engine::Progress;
use crate::spec::Spec;
use crate::{config, lane};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod case;
mod commit;
mod config_view;
mod diff;
mod fail;
mod keys;
mod progress;
mod review;
mod runs;
mod spec;
mod text;
mod theme;
mod widgets;

// The screens all reach for the same primitives, so they get one import
// surface: `use super::*` in a screen module. `keys` and `progress` are not
// re-exported — nothing needs to reach back into them.
pub use case::*;
pub use commit::*;
pub use config_view::*;
pub use diff::*;
pub use fail::*;
pub use review::*;
pub use runs::*;
pub use spec::*;
pub use text::*;
pub use theme::*;
pub use widgets::*;

pub fn run(verbose: bool) -> Result<i32> {
    // Uninitialised repos still get the TUI: init happens in-app (i on home).
    let repo = config::find_repo_root().or_else(|_| config::find_git_root())?;
    let mut app = App::new(repo, verbose);
    hand_back_on_panic();
    let mut terminal = enter();
    let res = app.event_loop(&mut terminal);
    leave();
    res?;
    Ok(0)
}

/// Give the terminal back if the thread that owns the screen dies. Only that
/// thread: an engine job panicking is caught by `pump` and reported as a failed
/// job, so tearing the screen down there would leave the UI drawing into the
/// live shell. Installed once, before the first `enter`, so the `$EDITOR` round
/// trip does not stack a hook per visit.
fn hand_back_on_panic() {
    let ui = std::thread::current().id();
    let next = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui {
            leave();
        }
        next(info);
    }));
}

/// Take the terminal, and ask for the kitty keyboard protocol while we have it:
/// without it ⇧↵ arrives as a plain ↵, and in a box where ↵ submits there is
/// then no way to type a newline at all. Best effort — a terminal that doesn't
/// understand the escape ignores it, and alt+↵ still works there.
///
/// Spelled out rather than `ratatui::init`, whose panic hook fires on every
/// thread and does not know about the keyboard flags. `hand_back_on_panic` owns
/// that job.
fn enter() -> DefaultTerminal {
    use ratatui::crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    enable_raw_mode().expect("enable raw mode");
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
        .expect("terminal")
}

/// Give it back. Paired with `enter`, because the keyboard flags outlive the
/// alt screen and a shell that inherits them gets strange keys.
fn leave() {
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::PopKeyboardEnhancementFlags
    );
    ratatui::restore();
}

// ---- app state -----------------------------------------------------------

pub enum JobKind {
    Plan,
    Run,
    Fix,
    /// Drafting a commit message: the only job that doesn't own the screen.
    Draft,
}

pub enum LogItem {
    Stage(String),
    Gate { gate: String, ok: bool, detail: String },
}

pub enum Outcome {
    /// Carries the lane's closing message. Most callers ignore it; a drafted
    /// commit message is the whole point of the job that produced it.
    Done(String),
    /// The detail is not carried: `fail()` wrote it to events.ndjson before it
    /// sent this, and the Failure tab reads it from there — so a reopened run
    /// shows exactly what a fresh one does.
    Failed { why: String },
    Error(String),
}

pub struct Job {
    pub kind: JobKind,
    pub run_id: Option<String>,
    pub rx: Receiver<Progress>,
    pub handle: Option<JoinHandle<Result<i32>>>,
    pub started: Instant,
    pub log: Vec<LogItem>,
    pub lane: String,
    pub tail: VecDeque<String>,
    pub denials: usize,
    pub tools: usize, // tool calls seen across lanes (live activity signal)
    pub outcome: Option<Outcome>,
}

pub enum Screen {
    Runs,
    Progress,
    /// The one screen a run has, at every stage: `TABS` is the journey and the
    /// strip greys out what has not happened yet.
    ///
    /// Boxed for the same reason its own `review` is: `Screen` is as big as its
    /// biggest variant, and this variant is eight times the size of the next.
    Case(Box<CaseView>),
    /// The end of the road for a run: staged in your tree, or committed. Two
    /// different endings that both mean "guvnor is done and it's yours now".
    Landed { title: &'static str, msg: String },
}

pub enum Go {
    Quit,
    Runs,
    Case(String),
    CaseTab(String, usize), // reopen the case file on a specific tab
    Plan(String, String),
    Replan(String, String),
    Run(String),
    Fix(String, Vec<crate::review::Finding>, String),
    Progress,
    Edit(String, PathBuf),
    /// The stage box's actions, off the Review tab: apply to / back out of the
    /// working tree, or open the commit-message modal. They rebuild the run
    /// screen, so they run in `apply`, not inline in a key handler.
    Stage(String),
    Unstage(String),
    OpenCommit(String),
    /// Draft a commit message. Unlike every other job this one does NOT leave
    /// the screen: the modal that asked for it stays up and fills itself in.
    Draft(String),
    Landed { title: &'static str, msg: String },
}

pub struct App {
    pub repo: PathBuf,
    pub verbose: bool,
    pub initialized: bool,
    pub cfg_models: Option<[String; 3]>, // planner/worker/reviewer for the config box
    pub runs: Vec<RunRow>,
    pub table: TableState,
    pub screen: Screen,
    pub job: Option<Job>,
    pub toast: Option<(String, Instant)>,
    pub help: bool,
    pub config: Option<ConfigView>,
    /// The commit modal, over the run screen. `Some` = it has the keyboard.
    pub commit: Option<CommitView>,
    pub confirm_delete: Option<(String, String, Buttons)>, // id, title, actions
    pub filter: LineInput,
    pub filtering: bool,
    /// The new-feature panel is a permanent box on the home screen; `focus`
    /// says whether it or the runs list currently has the keyboard.
    pub new: NewView,
    pub focus: HomeFocus,
}

#[cfg(test)]
impl App {
    /// A bare app for key-handler tests: no repo scan, no modals, nothing on
    /// disk. `App::new` reloads the run list and can open the config modal,
    /// which is not what a keystroke test is about.
    pub fn for_test() -> Self {
        Self {
            repo: PathBuf::from("/nonexistent"),
            verbose: false,
            initialized: true,
            cfg_models: None,
            runs: Vec::new(),
            table: TableState::default(),
            screen: Screen::Runs,
            job: None,
            toast: None,
            help: false,
            config: None,
            commit: None,
            confirm_delete: None,
            filter: LineInput::default(),
            filtering: false,
            new: NewView::default(),
            focus: HomeFocus::Runs,
        }
    }
}

pub fn toast(msg: impl Into<String>) -> Option<(String, Instant)> {
    Some((msg.into(), Instant::now()))
}

/// Append to the bounded lane-feed buffer, dropping the oldest line past the cap.
fn push_capped(tail: &mut VecDeque<String>, line: String) {
    if tail.len() >= 200 {
        tail.pop_front();
    }
    tail.push_back(line);
}

pub fn render_help(f: &mut Frame, area: Rect) {
    // Not a keymap: every box already prints the keys that act on it along its
    // own border, and a red glyph always means "press this". What a border
    // can't say is what the thing IS, and in what order you meet them — so
    // this page is the tour, one line per stop, and only the key that gets
    // you there.
    let head = Style::new().fg(Color::Yellow).bold();
    let key = Style::new().fg(Color::Red).bold();
    let dim = Style::new().fg(Color::DarkGray);
    // `label  description`, in two aligned columns so nothing wraps ragged
    let row = |label: &str, style: Style, what: &str| {
        Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{label:<10}"), style),
            Span::raw(what.to_string()),
        ])
    };
    let mut lines = vec![Line::raw("")];
    let section = |lines: &mut Vec<Line>, title: &str, sub: &str| {
        lines.push(Line::styled(format!("  {title}"), head));
        if !sub.is_empty() {
            lines.push(Line::styled(format!("    {sub}"), dim));
        }
    };
    section(&mut lines, "the loop", "you hold the gates · the models type · the evidence decides");
    for (k, what) in [
        ("n", "describe a feature — the planner writes a spec"),
        ("↵", "read that spec and approve it: nothing runs until you do"),
        ("r", "run: test-writer → RED gate → implementer → GREEN gate → reviewer"),
        ("↵", "triage the reviewer's findings, then approve Tests and Work"),
        ("s", "stage: the patches land in your tree and guvnor STOPS — go look"),
        ("↵", "commit — or unstage. guvnor never pushes; the remote is yours."),
    ] {
        lines.push(row(k, key, what));
    }
    lines.push(Line::raw(""));
    section(&mut lines, "screens", "");
    for (name, what) in [
        ("runs", "home: the run list, the new-feature panel, the config box"),
        ("progress", "the live lane feed while a run works — esc backgrounds it"),
        ("run", "one run as five tabs; ←/→ moves, esc goes home"),
    ] {
        lines.push(row(name, Style::new().bold(), what));
    }
    lines.push(Line::raw(""));
    section(&mut lines, "the run's tabs", "");
    for (name, what) in [
        ("Spec", "the proposal: goal, criteria, constraints. e edits it, i sends the"),
        ("", "planner feedback, ↵ approves — this gate holds everything back."),
        ("Tests", "the test-writer's patch. It counts only because it was RED on base."),
        ("Work", "the implementer's patch. It counts because the suite went GREEN."),
        ("Review", "a higher-tier model's verdict, its findings, the cost — and the"),
        ("", "stage box at its foot (s), muted until all three gates are green."),
        ("Failure", "there only while a run is broken: what broke, and what to do."),
        ("greyed", "a tab that hasn't happened yet. Never a destination."),
    ] {
        lines.push(row(name, Style::new().bold(), what));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  the keys for whatever you're looking at are on its border.",
        dim,
    ));
    lines.push(Line::raw(""));
    let w = 100.min(area.width.saturating_sub(4));
    let tw = w.saturating_sub(2).max(1) as usize; // inner text width
    let wrapped: usize = lines.iter().map(|l| l.width().max(1).div_ceil(tw)).sum();
    let h = (wrapped as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            boxed("help — any key closes", Style::new().bold()),
        ),
        popup,
    );
}

pub fn edit_in_editor(path: &std::path::Path) {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else { return };
    let _ = std::process::Command::new(bin).args(parts).arg(path).status();
}

impl App {

    pub fn new(repo: PathBuf, verbose: bool) -> Self {
        let mut app = Self {
            repo,
            verbose,
            initialized: false,
            cfg_models: None,
            runs: Vec::new(),
            table: TableState::default(),
            screen: Screen::Runs,
            job: None,
            toast: None,
            help: false,
            config: None,
            commit: None,
            confirm_delete: None,
            filter: LineInput::default(),
            filtering: false,
            new: NewView::default(),
            focus: HomeFocus::Runs,
        };
        app.reload_runs();
        if !app.initialized {
            // greet uninitialised repos with the config modal (in-app init)
            app.config = Some(ConfigView::from_repo(&app.repo));
        }
        app
    }

    pub fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.pump();
            self.maybe_finish();
            if let Some((_, t)) = &self.toast {
                if t.elapsed() > Duration::from_secs(4) {
                    self.toast = None;
                }
            }
            terminal.draw(|f| self.render(f))?;
            if !event::poll(Duration::from_millis(120))? {
                continue;
            }
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match self.handle_key(&key) {
                Some(Go::Quit) => return Ok(()),
                Some(Go::Edit(id, path)) => {
                    leave();
                    edit_in_editor(&path);
                    *terminal = enter();
                    terminal.clear()?;
                    if let Err(e) = Spec::load(&path) {
                        self.toast = toast(format!("spec invalid after edit: {e:#}"));
                    }
                    self.apply(Go::CaseTab(id, 0));
                }
                Some(go) => self.apply(go),
                None => {}
            }
        }
    }

    // ---- engine plumbing ------------------------------------------------

    pub fn start_job<F>(&mut self, kind: JobKind, run_id: Option<String>, op: F)
    where
        F: FnOnce(&Sender<Progress>) -> Result<i32> + Send + 'static,
    {
        if self.job.is_some() {
            self.toast = toast("a job is already running (v to watch, c to cancel)");
            return;
        }
        lane::reset_cancel();
        let (tx, rx) = std::sync::mpsc::channel();
        // Every job but a draft owns the screen while it runs. A draft was asked
        // for from a modal and has to hand its answer back to that modal, so the
        // modal has to still be there.
        let stay = matches!(kind, JobKind::Draft);
        let handle = std::thread::spawn(move || op(&tx));
        self.job = Some(Job {
            kind,
            run_id,
            rx,
            handle: Some(handle),
            started: Instant::now(),
            log: Vec::new(),
            lane: String::new(),
            tail: VecDeque::new(),
            denials: 0,
            tools: 0,
            outcome: None,
        });
        if !stay {
            self.screen = Screen::Progress;
        }
    }

    pub fn pump(&mut self) {
        let Some(job) = self.job.as_mut() else { return };
        loop {
            match job.rx.try_recv() {
                Ok(p) => match p {
                    Progress::Stage(s) => job.log.push(LogItem::Stage(s)),
                    Progress::RunCreated { id } => job.run_id = Some(id),
                    Progress::LaneLine { lane, line } => {
                        if line.contains("guvnor: BLOCKED") {
                            job.denials += 1;
                        }
                        if job.lane != lane {
                            job.lane = lane;
                            // stage divider so lane switches stand out in the feed
                            push_capped(&mut job.tail, format!("── {} ──", job.lane));
                        }
                        for l in lane_display(&line) {
                            if l.starts_with("→ ") {
                                job.tools += 1;
                            }
                            push_capped(&mut job.tail, l);
                        }
                    }
                    Progress::GateResult { gate, ok, detail } => {
                        job.log.push(LogItem::Gate { gate, ok, detail })
                    }
                    Progress::Done(m) => job.outcome = Some(Outcome::Done(m)),
                    Progress::Failed { why, .. } => {
                        job.outcome = Some(Outcome::Failed { why })
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(h) = job.handle.take() {
                        match h.join() {
                            Ok(Ok(_)) => {
                                job.outcome.get_or_insert(Outcome::Done(String::new()));
                            }
                            Ok(Err(e)) => {
                                job.outcome.get_or_insert(Outcome::Error(format!("{e:#}")));
                            }
                            Err(_) => {
                                job.outcome
                                    .get_or_insert(Outcome::Error("engine thread panicked".into()));
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    pub fn maybe_finish(&mut self) {
        let finished = matches!(&self.job, Some(j) if j.outcome.is_some() && j.handle.is_none());
        if !finished {
            return;
        }
        let job = self.job.take().unwrap();
        self.reload_runs();
        let on_progress = matches!(self.screen, Screen::Progress);
        match (job.kind, job.outcome.unwrap()) {
            (JobKind::Plan, Outcome::Done(_)) => {
                if on_progress {
                    match job.run_id {
                        Some(id) => self.apply(Go::CaseTab(id, 0)),
                        None => self.apply(Go::Runs),
                    }
                } else {
                    self.toast = toast("plan finished — spec ready to read");
                }
            }
            // Triage the reviewer's findings BEFORE reading the diffs: decide
            // what still needs fixing, then judge what's left. A fix round
            // re-reviews, so it lands back here on the fresh findings.
            (JobKind::Run | JobKind::Fix, Outcome::Done(_)) => {
                if on_progress {
                    match job.run_id {
                        Some(id) => self.apply(Go::CaseTab(id, REVIEW_TAB)),
                        None => self.apply(Go::Runs),
                    }
                } else {
                    self.toast = toast("run finished — review ready");
                }
            }
            // Back into the modal that asked, never onto another screen.
            (JobKind::Draft, Outcome::Done(msg)) => match self.commit.as_mut() {
                Some(v) => v.set_message(&msg),
                // modal closed while it was thinking: the words are still worth
                // having, and losing them silently would be the worse bug
                None => self.toast = toast("commit message drafted — reopen with c"),
            },
            (JobKind::Draft, out) => {
                if let Some(v) = self.commit.as_mut() {
                    v.drafting = false;
                }
                self.toast = toast(match out {
                    Outcome::Error(e) => e,
                    _ => "could not draft a commit message".into(),
                });
            }
            // `fail()` wrote why+detail to events.ndjson before sending this, so
            // the Failure tab reads the same evidence whether you land on it now
            // or reopen the run tomorrow.
            (_, Outcome::Failed { why }) => match job.run_id {
                Some(id) if on_progress => self.apply(Go::CaseTab(id, FAIL_TAB)),
                _ => self.toast = toast(format!("run failed [{why}] — open it for evidence")),
            },
            // An `Err` return is a precondition, not a lane outcome: nothing ran,
            // nothing is on disk, and the message is one actionable line.
            (_, Outcome::Error(e)) => {
                self.toast = toast(e);
                if on_progress {
                    self.apply(Go::Runs);
                }
            }
        }
    }

    // ---- rendering ---------------------------------------------------------

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        // single source of key hints: the outer border bottom (toast takes it over)
        // popups carry their own hints on their bottom borders — outer goes quiet
        let pairs: Vec<(&str, &str)> = match &self.screen {
            Screen::Runs if self.config.is_some() || self.confirm_delete.is_some() => vec![],
            Screen::Runs if self.filtering => vec![("↵", "apply"), ("esc", "clear")],
            // the new-feature panel carries its own hints on its border
            Screen::Runs if self.focus == HomeFocus::New => vec![],
            // runs/config actions live on their box borders; bottom keeps globals
            Screen::Runs => vec![("?", "help"), ("q", "quit")],
            Screen::Progress => vec![("v", "verbose"), ("c", "cancel"), ("esc", "background")],
            Screen::Case(_) if self.commit_open() => vec![],
            Screen::Case(v) if v.feedback.is_some() => vec![],
            Screen::Case(v) => {
                if v.note.is_some() {
                    vec![("↵", "confirm reject"), ("esc", "cancel note")]
                } else if v.confirm.is_some() {
                    vec![]
                } else {
                    let mut h = vec![("←/→", "tabs")];
                    match v.tab {
                        REVIEW_TAB => {
                            // the red letters in the box titles say the rest;
                            // ↵ needs no advertising — the cursor is on the
                            // thing it acts on
                            h.push(("f", "findings"));
                            h.push(("r", "reviewer"));
                            h.push(("t", "tokens"));
                        }
                        FAIL_TAB => {}
                        t if !v.approved[t] => h.push(("↵", "judge")),
                        _ => {}
                    }
                    // Changing the spec belongs where the spec is.
                    if v.tab == 0 {
                        h.push(("e", "edit"));
                        h.push(("i", "iterate"));
                    }
                    // `r` is the run key, and it is the *only* thing to do on a
                    // spec that has been approved and never run — the next-step
                    // line above says as much.
                    if v.approved[0] && v.tab != REVIEW_TAB {
                        h.push(("r", if v.live.contains(&1) { "re-run" } else { "run" }));
                    }
                    // `s` jumps to the stage box on the Review tab. Advertised
                    // once a review exists; the box itself stays muted until
                    // every gate is green.
                    if v.live.contains(&REVIEW_TAB) {
                        h.push(("s", if v.staged { "commit" } else { "stage" }));
                    }
                    h.push(("esc", "back"));
                    h
                }
            }
            Screen::Landed { .. } => vec![("any key", "runs")],
        };
        let bottom = match &self.toast {
            Some((msg, _)) => Line::from(Span::styled(
                format!(" {msg} "),
                Style::new().fg(Color::Black).bg(Color::Yellow),
            )),
            None => hint_line(&pairs),
        };
        let outer = boxed("Guv'nor", Style::new().bold()).title_bottom(bottom);
        let inner = outer.inner(area);
        f.render_widget(outer, area);
        match &self.screen {
            Screen::Runs => {
                self.render_runs(f, inner);
                self.render_runs_popups(f, inner);
            }
            Screen::Progress => self.render_progress(f, inner),
            Screen::Case(_) => {
                self.render_case(f, inner);
                self.render_commit(f, inner);
            }
            Screen::Landed { .. } => self.render_landed(f, inner),
        }
        if self.help {
            render_help(f, inner);
        }
    }

}
