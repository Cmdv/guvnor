//! Every keystroke and every navigation, in one place: `handle_key` decides
//! what a key means on the current screen, `apply` carries out the move.

use crate::engine::{self};
use crate::state::{self};
use crate::lane;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

use super::*;

impl App {

    // ---- keys -------------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> Option<Go> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.job.is_some() {
                self.toast = toast("job running — c to cancel it first, then quit");
                return None;
            }
            return Some(Go::Quit);
        }
        if self.help {
            self.help = false;
            return None;
        }
        // '?' opens help everywhere except while typing (inputs, filter, feedback).
        let typing = (matches!(&self.screen, Screen::Runs) && self.focus == HomeFocus::New)
            || matches!(&self.screen, Screen::Case(v) if v.note.is_some() || v.feedback.is_some())
            || self.filtering
            || self.config.is_some();
        if !typing && key.code == KeyCode::Char('?') {
            self.help = true;
            return None;
        }
        match &mut self.screen {
            Screen::Runs => {
                // config modal: everything guvnor.toml holds
                if self.config.is_some() {
                    // model version dropdown swallows keys first
                    if self.config.as_ref().unwrap().drop.is_some() {
                        let cv = self.config.as_mut().unwrap();
                        match key.code {
                            KeyCode::Esc => cv.drop = None,
                            KeyCode::Enter => {
                                if let Some((sel, options)) = cv.drop.take() {
                                    let seat = cv.row - 5;
                                    cv.models[seat] = options[sel].clone();
                                }
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                if let Some((sel, options)) = cv.drop.as_mut() {
                                    *sel = (*sel + 1).min(options.len() - 1);
                                }
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                if let Some((sel, _)) = cv.drop.as_mut() {
                                    *sel = sel.saturating_sub(1);
                                }
                            }
                            _ => {}
                        }
                        return None;
                    }
                    match key.code {
                        KeyCode::Esc => self.config = None,
                        KeyCode::Up | KeyCode::BackTab => {
                            let cv = self.config.as_mut().unwrap();
                            cv.row = cv.row.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Tab => {
                            let cv = self.config.as_mut().unwrap();
                            cv.row = (cv.row + 1).min(CFG_ROWS - 1);
                        }
                        // action row is the last one: ↵ saves or closes
                        _ if self.config.as_ref().unwrap().row == CFG_ROWS - 1 => {
                            let cv = self.config.as_mut().unwrap();
                            match cv.buttons.handle(key.code) {
                                Some(0) => self.save_config(),
                                Some(_) => self.config = None,
                                None => {}
                            }
                        }
                        KeyCode::Enter => {
                            let cv = self.config.as_mut().unwrap();
                            if (5..=7).contains(&cv.row) {
                                cv.open_drop();
                            }
                        }
                        _ => {
                            let cv = self.config.as_mut().unwrap();
                            match cv.row {
                                0 => match key.code {
                                    KeyCode::Left | KeyCode::Char('h') => cv.stamp_preset(-1),
                                    KeyCode::Right | KeyCode::Char('l') => cv.stamp_preset(1),
                                    _ => {}
                                },
                                4 => match key.code {
                                    KeyCode::Left | KeyCode::Char('h') => cv.stamp_model_preset(-1),
                                    KeyCode::Right | KeyCode::Char('l') => cv.stamp_model_preset(1),
                                    _ => {}
                                },
                                5..=7 => match key.code {
                                    KeyCode::Left | KeyCode::Char('h') => {
                                        cv.models[cv.row - 5] = cycle_model(&cv.models[cv.row - 5], -1)
                                    }
                                    KeyCode::Right | KeyCode::Char('l') => {
                                        cv.models[cv.row - 5] = cycle_model(&cv.models[cv.row - 5], 1)
                                    }
                                    _ => {}
                                },
                                row => {
                                    if let Some(input) = cv.text_input(row) {
                                        input.handle(key);
                                    }
                                }
                            }
                        }
                    }
                    return None;
                }
                // delete confirmation popup: `cancel` sits at index 0, so the
                // reflex ↵ on a destructive prompt is the safe answer
                if let Some((id, title, buttons)) = self.confirm_delete.as_mut() {
                    if key.code == KeyCode::Esc {
                        self.confirm_delete = None;
                        return None;
                    }
                    match buttons.handle(key.code) {
                        Some(0) => self.confirm_delete = None,
                        Some(_) => {
                            let (id, title) = (id.clone(), title.clone());
                            self.confirm_delete = None;
                            let res = state::resolve_run_dir(&self.repo, &id)
                                .and_then(|d| std::fs::remove_dir_all(d).map_err(Into::into));
                            match res {
                                Ok(()) => {
                                    self.toast = toast(format!("deleted '{title}'"));
                                    self.reload_runs();
                                }
                                Err(e) => self.toast = toast(format!("{e:#}")),
                            }
                        }
                        None => {}
                    }
                    return None;
                }
                // filter input mode (btop-style)
                if self.filtering {
                    match key.code {
                        KeyCode::Esc => {
                            self.filter = LineInput::default();
                            self.filtering = false;
                            self.clamp_selection();
                        }
                        KeyCode::Enter => self.filtering = false,
                        KeyCode::Down => self.move_selection(1),
                        KeyCode::Up => self.move_selection(-1),
                        _ => {
                            self.filter.handle(key);
                            self.clamp_selection();
                        }
                    }
                    return None;
                }
                // the new-feature panel has the keyboard: it swallows every key
                // (so typing a title never fires a runs action).
                if self.focus == HomeFocus::New {
                    return self.new_box_key(key);
                }
                match key.code {
                    KeyCode::Char('q') => {
                        if self.job.is_some() {
                            self.toast = toast("job running — c to cancel it first, then quit");
                            None
                        } else {
                            Some(Go::Quit)
                        }
                    }
                    // hop onto the new-feature panel: `n` and tab enter at the
                    // title, shift-tab at the context (so the cycle reverses).
                    KeyCode::Char('n') | KeyCode::Tab => {
                        self.focus = HomeFocus::New;
                        self.new.focus = 0;
                        None
                    }
                    KeyCode::BackTab => {
                        self.focus = HomeFocus::New;
                        self.new.focus = 1;
                        None
                    }
                    KeyCode::Char('f') => {
                        self.filtering = true;
                        self.filter.cursor = char_count(&self.filter.value);
                        None
                    }
                    KeyCode::Char('c') | KeyCode::Char('i') => {
                        self.config = Some(ConfigView::from_repo(&self.repo));
                        None
                    }
                    KeyCode::Char('d') => {
                        let sel = self
                            .selected_run()
                            .map(|r| (r.id.clone(), r.title.clone(), r.status.clone()));
                        if let Some((id, title, status)) = sel {
                            // A committed run's evidence is the record behind a
                            // commit that already exists — that one stays. Every
                            // other run is yours to bin.
                            if status == state::Status::Committed {
                                self.toast = toast("that run is committed — its evidence is the record");
                            } else {
                                self.confirm_delete =
                                    Some((id, title, Buttons::new(&["cancel", "delete"], &[Color::Gray, Color::Red])));
                            }
                        }
                        None
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.move_selection(1);
                        None
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.move_selection(-1);
                        None
                    }
                    KeyCode::Enter => {
                        let id = self.selected_run().map(|r| r.id.clone())?;
                        if let Some(j) = &self.job {
                            if j.run_id.as_deref() == Some(&id) {
                                return Some(Go::Progress);
                            }
                        }
                        // One screen per run, whatever stage it is at: the tab
                        // strip is the journey and it is always readable.
                        Some(Go::Case(id))
                    }
                    // No `r` here: starting a run belongs on the run's own
                    // screen, next to the spec it would be running.
                    _ => None,
                }
            }
            Screen::Progress => match key.code {
                KeyCode::Esc => Some(Go::Runs),
                KeyCode::Char('v') => {
                    self.verbose = !self.verbose;
                    None
                }
                KeyCode::Char('c') => {
                    lane::request_cancel();
                    self.toast = toast("cancel requested — killing lane process group");
                    None
                }
                _ => None,
            },
            Screen::Case(_) => {
                // The commit modal writes to your repo: while it is up it takes
                // every key, so nothing behind it can act by accident.
                if self.commit_open() {
                    return commit_key(self, key);
                }
                let Screen::Case(v) = &mut self.screen else { unreachable!() };
                // Spec iteration: the planner's feedback box takes every key
                // while it is open, same as the note and the commit modal.
                if let Some(p) = v.feedback.as_mut() {
                    // ↵ sends it wherever the cursor is; ⇧↵ is the newline.
                    let send = key.code == KeyCode::Enter
                        && !key.modifiers.intersects(newline_mods())
                        && (!p.on_buttons || p.buttons.sel == 0);
                    if send {
                        let text = p.text.value().trim().to_string();
                        if text.is_empty() {
                            self.toast = toast("feedback is empty");
                            return None;
                        }
                        return Some(Go::Replan(v.id.clone(), text));
                    }
                    match key.code {
                        KeyCode::Esc => v.feedback = None,
                        KeyCode::Tab | KeyCode::BackTab => p.on_buttons = !p.on_buttons,
                        // ↵ on `send` was taken above, so the only button left
                        // to fire is `cancel`
                        _ if p.on_buttons => {
                            if p.buttons.handle(key.code).is_some() {
                                v.feedback = None;
                            }
                        }
                        _ => p.text.handle(key),
                    }
                    return None;
                }
                if let Some(note) = &mut v.note {
                    return match key.code {
                        KeyCode::Esc => {
                            v.note = None;
                            None
                        }
                        KeyCode::Enter => {
                            match engine::set_gate(&v.id, tab_gate(v.tab), &note.value, false) {
                                Ok(m) => {
                                    self.toast = toast(m);
                                    Some(Go::Runs)
                                }
                                Err(e) => {
                                    self.toast = toast(format!("{e:#}"));
                                    None
                                }
                            }
                        }
                        _ => {
                            note.handle(key);
                            None
                        }
                    };
                }
                // approve/reject popup: approve preselected, so ↵↵ approves.
                // The re-run popup preselects `cancel` — it spends money.
                if let Some((ask, buttons)) = v.confirm.as_mut() {
                    if key.code == KeyCode::Esc {
                        v.confirm = None;
                        return None;
                    }
                    let (ask, hit) = (*ask, buttons.handle(key.code));
                    if ask == Ask::Rerun {
                        return match hit {
                            None => None, // ←/→ moved, nothing fired
                            Some(0) => {
                                v.confirm = None; // cancel
                                None
                            }
                            Some(_) => {
                                let id = v.id.clone();
                                v.confirm = None;
                                Some(Go::Run(id))
                            }
                        };
                    }
                    return match hit {
                        Some(0) => {
                            v.confirm = None;
                            match engine::set_gate(&v.id, tab_gate(v.tab), "", true) {
                                Ok(m) => {
                                    self.toast = toast(m);
                                    // stay in flow: advance to the next thing to judge
                                    Some(Go::CaseTab(v.id.clone(), (v.tab + 1).min(REVIEW_TAB)))
                                }
                                Err(e) => {
                                    self.toast = toast(format!("{e:#}"));
                                    None
                                }
                            }
                        }
                        Some(_) => {
                            v.confirm = None;
                            v.note = Some(LineInput::default());
                            None
                        }
                        None => None,
                    };
                }
                // The Review tab has its own control surface (tick a finding,
                // type an instruction, fire the fix round), so it takes the keys
                // ←/→ and esc excepted: tab navigation has to work the same on
                // all four tabs, or the last one becomes a trap.
                if v.tab == REVIEW_TAB && key.code != KeyCode::Esc {
                    if let Some(r) = v.review.as_deref_mut() {
                        match review_key(r, key) {
                            Took::Go(go) => return Some(go),
                            Took::Say(m) => {
                                self.toast = toast(m);
                                return None;
                            }
                            Took::Yes => return None,
                            Took::No => {}
                        }
                    }
                }
                let Screen::Case(v) = &mut self.screen else { return None };
                match key.code {
                    KeyCode::Esc => Some(Go::Runs),
                    KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                        v.step(1);
                        None
                    }
                    KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                        v.step(-1);
                        None
                    }
                    // On the Spec tab the boxes own the arrows and the digits:
                    // each one scrolls its own content, so there is no single
                    // body scroll to drive.
                    _ if v.tab == 0 && v.spec.is_some() && v.panels.handle(key) => None,
                    // Tests and Work are file lists: the cursor and the fold are
                    // theirs, everything else falls through to the run's keys.
                    _ if (1..=2).contains(&v.tab) && v.diffs[v.tab - 1].handle(key) => None,
                    KeyCode::Char('j') | KeyCode::Down => scrolled(&mut v.scroll, 1),
                    KeyCode::Char('k') | KeyCode::Up => scrolled(&mut v.scroll, -1),
                    KeyCode::PageDown => scrolled(&mut v.scroll, 10),
                    KeyCode::PageUp => scrolled(&mut v.scroll, -10),
                    // The two ways to change a spec, and they only make sense
                    // where the spec is: by hand, or by asking the planner.
                    // (`change the spec` on the Review tab is the same replan.)
                    KeyCode::Char('e') if v.tab == 0 => {
                        Some(Go::Edit(v.id.clone(), v.dir.join("spec.json")))
                    }
                    KeyCode::Char('i') if v.tab == 0 => {
                        v.feedback = Some(Prompt::default());
                        None
                    }
                    // Review and Failure are not gates — nothing to judge. (Only
                    // the three gate tabs are < REVIEW_TAB.)
                    KeyCode::Enter if v.tab < REVIEW_TAB => {
                        v.confirm = Some((Ask::Gate, Buttons::new(&["approve", "reject"], YES_NO)));
                        None
                    }
                    // Start the lanes, or give a failed run another go. The
                    // engine re-checks spec approval and wipes the stale trees;
                    // saying so here saves a trip through a job that refuses.
                    KeyCode::Char('r') if !v.approved[0] => {
                        self.toast = toast("approve the spec first (↵ on the Spec tab) — it gates the run");
                        None
                    }
                    // A re-run throws away patches that exist and pays for three
                    // more lanes, so it asks first. The first run has nothing to
                    // lose and nothing to ask about.
                    KeyCode::Char('r') if v.live.contains(&1) => {
                        v.confirm = Some((Ask::Rerun, Buttons::new(&["cancel", "re-run"], YES_NO)));
                        None
                    }
                    KeyCode::Char('r') => Some(Go::Run(v.id.clone())),
                    // `s` jumps to the stage box at the foot of the Review tab
                    // and focuses it. It's muted until every gate is green, but
                    // it's still where you land — the review is where landing
                    // lives. (On the Review tab itself, review_key handles `s`.)
                    KeyCode::Char('s') if v.live.contains(&REVIEW_TAB) => {
                        v.tab = REVIEW_TAB;
                        if let Some(r) = v.review.as_deref_mut() {
                            r.focus = ReviewFocus::Stage;
                        }
                        None
                    }
                    KeyCode::Char('s') => {
                        self.toast = toast("nothing to stage yet — run the lanes and review first");
                        None
                    }
                    _ => None,
                }
            }
            Screen::Landed { .. } => Some(Go::Runs),
        }
    }

    /// Mirrors `handle_key`, for clicks. Only the Case screen's tab strip
    /// answers today; everything else is a deliberate `None`.
    pub fn handle_mouse(&mut self, m: &MouseEvent) -> Option<Go> {
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let pos = Position::new(m.column, m.row);
        if let Screen::Case(v) = &mut self.screen {
            if let Some(k) = hit_test(&v.tab_cells, pos) {
                v.goto(v.shown[k]);
            }
        }
        None
    }

    /// Keys for the new-feature panel while it holds focus. Tab walks title →
    /// context → back to the runs list; ↵ plans it (⇧↵ is the newline); esc
    /// hands focus back without leaving the home screen.
    fn new_box_key(&mut self, key: &KeyEvent) -> Option<Go> {
        match key.code {
            KeyCode::Esc => {
                self.focus = HomeFocus::Runs;
                None
            }
            KeyCode::Tab => {
                if self.new.focus == 0 {
                    self.new.focus = 1;
                } else {
                    self.focus = HomeFocus::Runs;
                }
                None
            }
            KeyCode::BackTab => {
                if self.new.focus == 1 {
                    self.new.focus = 0;
                } else {
                    self.focus = HomeFocus::Runs;
                }
                None
            }
            // ↵ plans it from either field; ⇧↵ is the newline in the context.
            KeyCode::Enter if !key.modifiers.intersects(newline_mods()) => {
                if self.new.title.value.trim().is_empty() {
                    self.toast = toast("title is empty");
                    return None;
                }
                Some(Go::Plan(
                    self.new.title.value.trim().to_string(),
                    self.new.context.value().trim().to_string(),
                ))
            }
            _ => {
                if self.new.focus == 0 {
                    self.new.title.handle(key);
                } else {
                    self.new.context.handle(key);
                }
                None
            }
        }
    }

    pub fn apply(&mut self, go: Go) {
        match go {
            Go::Runs => {
                self.reload_runs();
                self.screen = Screen::Runs;
            }
            Go::Case(id) => match self.build_case(&id) {
                Ok(v) => self.screen = Screen::Case(Box::new(v)),
                Err(e) => self.toast = toast(format!("{e:#}")),
            },
            Go::CaseTab(id, tab) => match self.build_case(&id) {
                Ok(mut v) => {
                    // A tab that isn't live can't be landed on — the run's state
                    // may have moved since whoever asked for it looked.
                    v.tab = if v.live.contains(&tab) { tab } else { 0 };
                    self.screen = Screen::Case(Box::new(v));
                }
                Err(e) => self.toast = toast(format!("{e:#}")),
            },
            Go::Plan(title, context) => {
                // the panel did its job — clear it and hand focus back to the list
                self.new = NewView::default();
                self.focus = HomeFocus::Runs;
                self.start_job(JobKind::Plan, None, move |tx| engine::plan(&title, &context, tx))
            }
            Go::Replan(id, feedback) => {
                let id2 = id.clone();
                self.start_job(JobKind::Plan, Some(id), move |tx| engine::replan(&id2, &feedback, tx))
            }
            Go::Run(id) => {
                let id2 = id.clone();
                self.start_job(JobKind::Run, Some(id), move |tx| engine::run(&id2, tx))
            }
            Go::Fix(id, findings, note) => {
                let id2 = id.clone();
                self.start_job(JobKind::Fix, Some(id), move |tx| {
                    engine::fix(&id2, &findings, &note, tx)
                })
            }
            Go::Progress => self.screen = Screen::Progress,
            // The stage box's actions: they touch the repo, then rebuild the run
            // screen back onto the Review tab with the box refocused. The git
            // work runs off the UI thread; `maybe_finish` does the rebuild.
            Go::Stage(id) => {
                let id2 = id.clone();
                self.toast = toast("staging …");
                self.start_job(JobKind::Land(Land::Stage), Some(id), move |tx| {
                    let msg = engine::stage(&id2)?;
                    let _ = tx.send(Progress::Done(msg));
                    Ok(0)
                })
            }
            Go::Unstage(id) => {
                let id2 = id.clone();
                self.toast = toast("unstaging …");
                self.start_job(JobKind::Land(Land::Unstage), Some(id), move |tx| {
                    let msg = engine::unstage(&id2)?;
                    let _ = tx.send(Progress::Done(msg));
                    Ok(0)
                })
            }
            Go::OpenCommit(id) => self.open_commit(&id),
            Go::Draft(id) => {
                let id2 = id.clone();
                self.start_job(JobKind::Draft, Some(id), move |tx| {
                    engine::commit_message(&id2, tx)
                })
            }
            Go::Landed { title, msg } => {
                self.reload_runs();
                self.screen = Screen::Landed { title, msg };
            }
            Go::Quit | Go::Edit(..) => unreachable!("handled in event_loop"),
        }
    }

}
