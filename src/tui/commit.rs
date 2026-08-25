//! Landing: the `StageView` box (rendered on the Review tab, see `review.rs`)
//! and the commit-message modal it can open.
//!
//! Two steps, deliberately. `stage` applies the change to your working tree and
//! stops, because until it is in your project you cannot open the files, run the
//! thing, or change your mind — `commit` and `unstage` are what you do after
//! looking. So the box carries the file list and the one or two buttons the tree
//! is in a state for, and writing a commit message is a modal on top: a
//! different job, asked for separately.
//!
//! Guvnor's commit is bound to the staged tree, so what it signs is what a
//! reviewer read. Guv'nor never pushes. Nothing in here can.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::state::Status;

use super::*;

/// Subject lines longer than this get folded in every git log, every PR title
/// and every terminal. The generator is told the same number.
pub const SUBJECT_MAX: usize = 80;

/// Which box the modal's keys act on. The message is a text field, so it has to
/// be possible to be somewhere else — otherwise `b` types a `b`.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum CommitFocus {
    Message,
    Actions,
}

/// The stage box (on the Review tab): what is about to enter your working tree,
/// or what already has, and the one or two things to do about it. No message box
/// — writing a commit message is a separate job, and it has its own modal.
pub struct StageView {
    pub files: Vec<String>,
    pub scroll: Scroll,
    pub buttons: Buttons,
    /// Applied to your working tree already.
    pub staged: bool,
    /// Committed: nothing left to do here, and nothing left to offer.
    pub done: bool,
    /// Staged files you have since edited without staging the edit. `git commit`
    /// takes the index, so those edits are not in it — worth saying, not worth
    /// refusing.
    pub edited: Vec<String>,
    /// The reviewer's verdict, when it was not a plain APPROVED. Said out loud
    /// at the point of no return; it does not stop anything — you approved the
    /// work gate having read it.
    pub verdict: Option<crate::review::Decision>,
}

impl StageView {
    pub fn build(id: &str, status: &Status, verdict: Option<crate::review::Decision>) -> Self {
        let staged = *status == Status::Staged;
        let done = *status == Status::Committed;
        Self {
            files: crate::engine::commit_files(id).unwrap_or_default(),
            scroll: Scroll::default(),
            // Exactly the moves the tree is in a state for. Committed offers
            // none: `Buttons` with no labels draws nothing and fires nothing.
            buttons: Buttons::new(
                match (done, staged) {
                    (true, _) => &[],
                    (_, true) => &["commit", "unstage"],
                    _ => &["stage"],
                },
                &[Color::Green, Color::Gray],
            ),
            verdict,
            staged,
            done,
            edited: if staged {
                crate::engine::unstaged_edits(id).unwrap_or_default()
            } else {
                Vec::new()
            },
        }
    }

    /// What is about to happen, in words.
    pub fn explain(&self) -> Vec<Line<'static>> {
        let dim = Style::new().fg(Color::DarkGray);
        let mut out = if self.done {
            vec![Line::styled(
                " committed. Guv'nor does not push — sending it anywhere is your call.",
                Style::new().fg(Color::Green),
            )]
        } else if self.staged {
            vec![
                Line::styled(
                    " These files are in your working tree now, staged and uncommitted.",
                    Style::new().fg(Color::Green),
                ),
                Line::styled(
                    " Open them, run them, read `git diff --cached`. Then commit, or take it",
                    dim,
                ),
                Line::styled(" back out with unstage — the run keeps all of its evidence.", dim),
            ]
        } else {
            vec![
                Line::raw(" Staging writes these files into your working tree and stops there,"),
                Line::styled(
                    " uncommitted, so you can open them and run them before deciding.",
                    dim,
                ),
                Line::styled(" Nothing is committed until you ask for it.", dim),
            ]
        };
        if let Some(v) = self.verdict.filter(|_| !self.done) {
            out.push(Line::styled(
                format!(" ⚠ the reviewer said {v} — you approved the work gate anyway, so this is yours"),
                Style::new().fg(Color::Yellow),
            ));
        }
        if !self.edited.is_empty() {
            out.push(Line::styled(
                format!(
                    " ⚠ edited since staging, so NOT in the commit: {}",
                    self.edited.join(", ")
                ),
                Style::new().fg(Color::Yellow),
            ));
        }
        out
    }
}

pub struct CommitView {
    pub id: String,
    /// Drawn and holding the keyboard. `esc` clears this but keeps the view, so
    /// a message you spent a minute on — or paid a lane to write — survives
    /// stepping out to re-read a diff.
    pub open: bool,
    pub msg: TextArea,
    /// Include the body, or commit the subject alone. A one-line commit is a
    /// legitimate choice and the generator always writes a body, so this is the
    /// only way to say "just the headline".
    pub with_body: bool,
    pub focus: CommitFocus,
    pub buttons: Buttons,
    /// A draft is being written. The modal stays up: you asked for a message,
    /// not for a trip to the progress screen.
    pub drafting: bool,
}

impl CommitView {
    pub fn new(id: String) -> Self {
        Self {
            id,
            open: true,
            msg: TextArea::default(),
            with_body: true,
            // The message is empty and that is the thing to fix first.
            focus: CommitFocus::Message,
            // Left to right is the order you'd use them in; `copy` is armed, not
            // `commit`, because a stray ↵ must never write git history or fire
            // the paid `generate` lane — landing a commit is the one move you
            // step over to on purpose.
            buttons: Buttons {
                labels: &["generate", "copy", "commit"],
                sel: 1,
                colors: &[Color::Gray, Color::Green, Color::Gray],
            },
            drafting: false,
        }
    }

    /// Subject and body as they will be committed — `with_body: false` drops the
    /// body rather than deleting it, so toggling back gets it again.
    pub fn parts(&self) -> (String, String) {
        let text = self.msg.value();
        let (subject, body) = crate::engine::split_commit_message(&text);
        (subject.to_string(), if self.with_body { body.to_string() } else { String::new() })
    }

    /// Fill the box from a generated draft, cursor at the end so it reads as
    /// something you can edit rather than something handed down.
    pub fn set_message(&mut self, text: &str) {
        self.msg = TextArea::from(text);
        self.drafting = false;
        self.focus = CommitFocus::Message;
    }
}

/// Keys for the commit modal. `Some(go)` navigates, `None` stays.
/// Every path out of here is explicit: esc, or a button.
pub fn commit_key(app: &mut App, key: &KeyEvent) -> Option<Go> {
    let v = app.commit.as_mut()?;
    // A draft in flight owns the modal: let it finish or cancel the whole thing.
    if v.drafting {
        if key.code == KeyCode::Esc {
            lane::request_cancel();
            v.drafting = false;
        }
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            v.open = false;
            return None;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            v.focus = match v.focus {
                CommitFocus::Message => CommitFocus::Actions,
                CommitFocus::Actions => CommitFocus::Message,
            };
            return None;
        }
        _ => {}
    }
    match v.focus {
        // A text field: everything types. ⇧↵ is the newline — the blank line
        // between subject and body is typed with it — and bare ↵ means "done
        // writing", which lands you on the actions with `commit` armed.
        CommitFocus::Message => {
            if key.code == KeyCode::Enter && !key.modifiers.intersects(newline_mods()) {
                v.focus = CommitFocus::Actions;
            } else {
                v.msg.handle(key);
            }
            None
        }
        CommitFocus::Actions => {
            if key.code == KeyCode::Char('b') {
                v.with_body = !v.with_body;
                return None;
            }
            // 0 generate · 1 copy · 2 commit — the index is the contract, the
            // labels are display only
            let i = v.buttons.handle(key.code)?;
            if i == 0 {
                v.drafting = true;
                return Some(Go::Draft(v.id.clone()));
            }
            // read out of the view before the arm that needs `app` itself
            let (subject, body) = v.parts();
            if i == 2 {
                return commit_now(app);
            }
            let full = if body.is_empty() { subject } else { format!("{subject}\n\n{body}") };
            // Checked before the copy, not after: `copy` is the armed button, so
            // a stray ↵ on a fresh modal used to hand pbcopy an empty string and
            // wipe the clipboard while reporting that nothing had happened.
            if full.trim().is_empty() {
                app.toast = toast("nothing to copy yet");
                return None;
            }
            app.toast = match clipboard(&full) {
                Ok(()) => toast("message copied to clipboard"),
                Err(e) => toast(e),
            };
            None
        }
    }
}

/// What a finished landing job leaves behind. The repo work already happened on
/// the job's thread; this is the redraw, so the box now offers `commit` and
/// `unstage` instead of `stage`, which is the whole point of stopping there.
///
/// ponytail: `reopen_stage` still costs two git calls on the UI thread. That is
/// down from the seven a synchronous stage ran, and the expensive one
/// (`git status`) moved with the job. Push the rebuild into the job too if a
/// large repo still drops frames here.
pub fn land_finished(app: &mut App, what: Land, id: &str, msg: String) {
    if what == Land::Unstage {
        // the draft describes a change that is no longer anywhere
        app.commit = None;
    }
    app.toast = toast(msg);
    app.reopen_stage(id);
}

/// The only thing here that writes history. Stages first if you skipped that,
/// so `commit` never means "nothing happened".
fn commit_now(app: &mut App) -> Option<Go> {
    let (id, subject, body) = {
        let v = app.commit.as_ref()?;
        let (s, b) = v.parts();
        (v.id.clone(), s, b)
    };
    if subject.is_empty() {
        app.toast = toast("write a subject line, or press generate");
        return None;
    }
    if char_count(&subject) > SUBJECT_MAX {
        app.toast = toast(format!(
            "subject is {} chars — keep it under {SUBJECT_MAX}",
            char_count(&subject)
        ));
        return None;
    }
    app.toast = toast("committing …");
    app.start_job(JobKind::Land(Land::Commit), Some(id.clone()), move |tx| {
        let msg = crate::engine::commit(&id, &subject, &body)?;
        let _ = tx.send(Progress::Done(msg));
        Ok(0)
    });
    None
}

impl App {
    /// Is the modal up? A hidden `CommitView` is a kept draft, not a modal —
    /// it must not eat keys or blank the hint bar.
    pub fn commit_open(&self) -> bool {
        self.commit.as_ref().is_some_and(|v| v.open)
    }

    /// Open the message modal. Reached only from the stage box's `commit`
    /// button, which exists only once the change is staged — so there is nothing
    /// left to guard here.
    pub fn open_commit(&mut self, id: &str) {
        // Same run: reopen the draft you left. A different one gets a fresh
        // view — a commit message is about one change, and showing another
        // run's words here would be the worst possible confusion.
        if let Some(v) = self.commit.as_mut().filter(|v| v.id == id) {
            v.open = true;
            return;
        }
        self.commit = Some(CommitView::new(id.to_string()));
    }

    pub fn render_commit(&self, f: &mut Frame, area: Rect) {
        let Some(v) = self.commit.as_ref().filter(|v| v.open) else { return };
        let [pc] = Layout::horizontal([Constraint::Percentage(72)]).flex(Flex::Center).areas(area);
        let [popup] = Layout::vertical([Constraint::Length(14)]).flex(Flex::Center).areas(pc);
        let block = modal(
            "commit message",
            &[("tab", "box"), ("b", "body"), ("⇧↵", "newline"), ("↵", "act"), ("esc", "cancel")],
        );
        let inner = block.inner(popup);
        f.render_widget(Clear, popup);
        f.render_widget(block, popup);

        // message · the line saying what will be committed · actions
        let [msg_a, what_a, btn_a] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(1), Constraint::Length(3)])
                .areas(inner);

        // The subject counter is the whole point of splitting the message: you
        // cannot see 80 characters by looking.
        let (subject, body) = v.parts();
        let n = char_count(&subject);
        let over = n > SUBJECT_MAX;
        let mbox = focus_box(
            "",
            &format!("message — subject {n}/{SUBJECT_MAX}"),
            v.focus == CommitFocus::Message,
        );
        let minner = mbox.inner(msg_a);
        f.render_widget(mbox, msg_a);
        if v.drafting {
            f.render_widget(
                Paragraph::new(Line::styled(
                    format!(" {} drafting a message — esc cancels", spin_frame()),
                    Style::new().fg(Color::Yellow),
                )),
                minner,
            );
        } else if v.msg.value().is_empty() && v.focus != CommitFocus::Message {
            f.render_widget(
                Paragraph::new(Line::styled(
                    " subject on line 1, blank line, then the body — or press generate",
                    Style::new().fg(Color::DarkGray),
                )),
                minner,
            );
        } else {
            // Subject in bold, and red the moment it stops fitting. Wrapped by
            // `wrap_line` rather than `Paragraph`, so the styling follows the
            // logical line across its rows and the cursor below lands on its
            // own glyph — the two agree because they share the break rule.
            let w = minner.width as usize;
            let mut text: Vec<Line> = Vec::new();
            for (i, l) in v.msg.lines.iter().enumerate() {
                let style = match i {
                    0 if over => Style::new().fg(Color::Red).bold(),
                    0 => Style::new().bold(),
                    _ if v.with_body => Style::new(),
                    // dropped from the commit: shown, so you can see what you
                    // are leaving out, but dimmed so you know it's out
                    _ => Style::new().fg(Color::DarkGray),
                };
                text.extend(wrap_line(l, w).into_iter().map(|(_, r)| Line::styled(r, style)));
            }
            let (cr, cc) = v.msg.wrapped(w).1;
            let yoff = (cr as u16).saturating_sub(minner.height.saturating_sub(1));
            f.render_widget(Paragraph::new(text).scroll((yoff, 0)), minner);
            if v.focus == CommitFocus::Message {
                f.set_cursor_position(Position::new(
                    minner.x + cc as u16,
                    minner.y + cr as u16 - yoff,
                ));
            }
        }

        let dim = Style::new().fg(Color::DarkGray);
        let body_note = if body.is_empty() {
            Span::styled("subject only", Style::new().fg(Color::Yellow))
        } else {
            Span::styled("subject + body", Style::new().fg(Color::Green))
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" commits the staged change · ", dim),
                body_note,
                Span::styled(" · guvnor never pushes", dim),
            ])),
            what_a,
        );
        v.buttons.render(f, btn_a, v.focus == CommitFocus::Actions);
    }

    /// Rebuild the run screen after staging or unstaging and land on the Review
    /// tab with the stage box focused, so the new state (and its new buttons)
    /// shows without a second keypress.
    pub fn reopen_stage(&mut self, id: &str) {
        match self.build_case(id) {
            Ok(mut v) => {
                if v.live.contains(&REVIEW_TAB) {
                    v.tab = REVIEW_TAB;
                    if let Some(r) = v.review.as_deref_mut() {
                        r.focus = ReviewFocus::Stage;
                    }
                }
                self.screen = Screen::Case(Box::new(v));
            }
            Err(e) => self.toast = toast(format!("{e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn a_message_splits_the_way_git_reads_it() {
        let mut v = CommitView::new("x".into());
        v.set_message("add rolling stats\n\nMean and median over a window,\nguarded against empty input.");
        let (s, b) = v.parts();
        assert_eq!(s, "add rolling stats");
        assert!(b.starts_with("Mean and median"), "{b:?}");
        // subject-only drops the body but keeps it in the box, so the toggle
        // is reversible — deleting the text would not be
        v.with_body = false;
        let (s2, b2) = v.parts();
        assert_eq!(s2, s);
        assert!(b2.is_empty());
        v.with_body = true;
        assert_eq!(v.parts().1, b, "the body must survive the round trip");
        // a one-liner has no body to find
        v.set_message("fix the thing");
        assert_eq!(v.parts(), ("fix the thing".into(), String::new()));
    }

    #[test]
    fn tab_reaches_every_box_and_comes_back() {
        let mut app = App::for_test();
        app.commit = Some(CommitView::new("x".into()));
        let focus = |a: &App| a.commit.as_ref().unwrap().focus;
        let start = focus(&app);
        for _ in 0..2 {
            commit_key(&mut app, &press(KeyCode::Tab));
        }
        assert!(focus(&app) == start, "two boxes, so two tabs come back round");
        // and backwards
        commit_key(&mut app, &press(KeyCode::Tab));
        commit_key(&mut app, &press(KeyCode::BackTab));
        assert!(focus(&app) == start);
        // esc closes the modal but keeps the draft: a message you spent a minute
        // on, or paid a lane to write, must survive stepping out to re-read a diff
        app.commit.as_mut().unwrap().set_message("add rolling stats\n\nwhy");
        commit_key(&mut app, &press(KeyCode::Esc));
        assert!(!app.commit_open(), "esc must close it");
        assert!(app.commit.is_some(), "esc must not delete the draft");
        app.open_commit("x");
        assert!(app.commit_open());
        assert_eq!(app.commit.as_ref().unwrap().parts().0, "add rolling stats");
        // a different run is a different change: never show it x's words
        app.open_commit("y");
        assert!(!matches!(&app.commit, Some(v) if v.id == "x"));
    }

    #[test]
    fn commit_refuses_an_empty_or_overlong_subject() {
        let mut app = App::for_test();
        app.commit = Some(CommitView::new("x".into()));
        // nothing typed: it must say so rather than write an empty commit
        assert!(commit_now(&mut app).is_none());
        assert!(app.toast.as_ref().unwrap().0.contains("subject"));
        // over the limit: refused with the count, not silently truncated —
        // truncating someone's commit message is worse than refusing it
        app.commit.as_mut().unwrap().set_message(&"x".repeat(SUBJECT_MAX + 1));
        app.toast = None;
        assert!(commit_now(&mut app).is_none());
        let msg = &app.toast.as_ref().unwrap().0;
        assert!(msg.contains(&(SUBJECT_MAX + 1).to_string()), "{msg}");
        // the run is untouched either way: nothing reached the repo
        assert!(app.commit.is_some());
        assert!(app.job.is_none(), "a refused subject must not reach the repo at all");
    }

    /// Staging spawns a handful of git processes, `git status` among them. Run on
    /// the UI thread that is a frozen screen with no frame drawn until it is
    /// over, so the three landing verbs go through a job like every other engine
    /// call. They keep the screen rather than taking it: each is brief and
    /// reports with a toast.
    #[test]
    fn the_landing_verbs_do_not_run_on_the_ui_thread() {
        for go in [Go::Stage("x".into()), Go::Unstage("x".into())] {
            let mut app = App::for_test();
            app.apply(go);
            assert!(app.job.is_some(), "the verb must be handed to a job");
            assert!(matches!(app.screen, Screen::Runs), "and must not take the screen");
            assert!(app.toast.is_some(), "with something on screen saying so");
        }
        // commit too, once it has a subject worth writing
        let mut app = App::for_test();
        let mut v = CommitView::new("x".into());
        v.set_message("add a thing");
        app.commit = Some(v);
        assert!(commit_now(&mut app).is_none());
        assert!(app.job.is_some(), "commit must be handed to a job as well");
    }

    #[test]
    fn the_message_modal_arms_copy_not_commit() {
        let v = CommitView::new("x".into());
        // a stray ↵ must never write history or fire the paid draft lane, so the
        // armed default is the harmless one — commit is stepped over to on purpose
        assert_eq!(v.buttons.labels, ["generate", "copy", "commit"]);
        assert_eq!(v.buttons.labels[v.buttons.sel], "copy");
    }

    /// The stage box offers exactly the moves the tree is in a state for, and
    /// nothing else — a button for something impossible is worse than no button.
    #[test]
    fn the_stage_box_offers_only_what_the_tree_allows() {
        let labels = |st: Status| StageView::build("nope", &st, None).buttons.labels.to_vec();
        assert_eq!(labels(Status::Reviewed), ["stage"], "not in the tree yet");
        assert_eq!(labels(Status::Staged), ["commit", "unstage"], "in the tree: keep it or not");
        assert!(labels(Status::Committed).is_empty(), "committed: nothing left to do");
        // ...and committed builds an empty row: `handle` answers, but there is
        // no label at any index and the dispatch guards on the same state
        let mut none = StageView::build("nope", &Status::Committed, None).buttons;
        assert_eq!(none.labels.get(none.handle(KeyCode::Enter).unwrap_or(0)), None);
    }

    /// The words under the file list are the whole explanation of why landing is
    /// two steps, so each state has to say something different and true.
    #[test]
    fn the_stage_box_explains_itself_in_each_state() {
        let text = |v: &StageView| lines_text(&v.explain());
        let words = |st: Status| text(&StageView::build("nope", &st, None));
        assert!(words(Status::Reviewed).contains("Staging writes these files"));
        assert!(words(Status::Reviewed).contains("Nothing is committed until you ask"));
        assert!(words(Status::Staged).contains("git diff --cached"));
        assert!(words(Status::Staged).contains("unstage"));
        assert!(words(Status::Committed).contains("does not push"));

        // an unstaged edit is named, since `git commit` will silently leave it out
        let mut v = StageView::build("nope", &Status::Staged, None);
        v.edited = vec!["src/a.js".into()];
        assert!(text(&v).contains("NOT in the commit") && text(&v).contains("src/a.js"));
    }

    #[test]
    fn landed_runs_are_named_in_the_past_tense() {
        // the run list and the tab strip read this: a staged run is not a
        // committed one, and neither is a failure
        assert_eq!(Status::Staged.to_string(), "staged");
        assert_eq!(Status::Committed.to_string(), "committed");
    }
}


