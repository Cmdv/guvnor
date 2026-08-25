//! The run screen: one tab per approval gate, then the reviewer's report,
//! then the failure if there is one.

use crate::spec::Spec;
use crate::state::{self, State, Status};
use crate::digest;
use anyhow::Result;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Clear, Paragraph, Tabs, Wrap,
};
use ratatui::Frame;

use super::*;

/// The whole journey of a run, in order, and the only screen a run has. One tab
/// per approval gate — each carrying the diff AND the evidence for the thing it
/// asks you to approve, so ↵ on a tab is a judgement on everything you just read
/// — then the reviewer's report. The report is evidence you read on the way to a
/// judgement, not a gate you hold, so it has no ✓ and no ↵; it sits in the strip
/// because that is where you look.
///
/// The strip is drawn whole from the moment a spec exists, with the tabs that
/// have nothing behind them yet greyed out (`CaseView::live` says which are
/// enterable) — it is the map of what happens to a feature, so it has to be
/// legible before any of it has happened. `Failure` is the exception: it is not
/// a stage of the journey, so it appears only when there is one.
///
/// Landing is NOT a tab. It lives as a box at the foot of the `Review` tab that
/// `s` jumps to and focuses; the box is muted until every gate is green. A whole
/// tab for one keypress would be a stop that goes nowhere.
pub const TABS: [&str; 5] = ["Spec", "Tests", "Work", "Review", "Failure"];

/// Tab with no gate behind it: the reviewer's report, read on the way to a
/// judgement, never approved.
pub const REVIEW_TAB: usize = 3;

/// Tab with no gate behind it: failure evidence, drawn only while the run is
/// broken.
pub const FAIL_TAB: usize = 4;

pub fn tab_gate(tab: usize) -> state::Gate {
    match tab {
        0 => state::Gate::Spec,
        1 => state::Gate::Tests,
        _ => state::Gate::Work,
    }
}

/// sha256 of `spec.json` as it sits on disk; `None` when unreadable.
fn spec_sha(dir: &std::path::Path) -> Option<String> {
    std::fs::read(dir.join("spec.json")).ok().map(|b| digest::sha256_hex(&b))
}

/// Whether the spec on disk no longer hashes to `pin` — the sha recorded when
/// a run cut its patches, or when the spec gate was approved. An empty pin
/// means nothing was recorded, so nothing has drifted.
fn spec_drifted(sha: Option<&str>, pin: &str) -> bool {
    !pin.is_empty() && sha.is_some_and(|s| s != pin)
}

/// What the confirm modal is asking. The gate ask judges the tab you're on;
/// the re-run ask guards three fresh lanes — it spends money and throws the
/// patches you already have away, so it is not a keystroke you make by accident.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Ask {
    Gate,
    Rerun,
}

pub struct CaseView {
    pub id: String,
    /// The run directory, for the one thing that needs a path rather than an
    /// id: opening `spec.json` in `$EDITOR`.
    pub dir: std::path::PathBuf,
    pub tab: usize,
    pub scroll: Scroll,
    pub note: Option<LineInput>,
    /// Spec iteration: feedback → planner revision.
    pub feedback: Option<Prompt>,
    /// Run name, then any warning flags. Left of the state, which sits hard
    /// right — so the gap between them is where a warning shows up.
    pub info: Line<'static>,
    /// The status chip, right-aligned on the same row.
    pub status: Line<'static>,
    /// The one thing to do next, always on screen.
    pub next: Line<'static>,
    /// The Spec tab's body when `spec` is `None` — the only case where that tab
    /// is prose rather than boxes.
    pub spec_lines: Vec<Line<'static>>,
    /// Tests (0) and Work (1) as collapsible file lists.
    pub diffs: [DiffView; 2],
    /// The Spec tab draws its boxes from this. `None` = spec.json unreadable,
    /// which `tabs[0]` says instead.
    pub spec: Option<Spec>,
    /// Cursor and scrolls for those boxes.
    pub panels: SpecPanels,
    /// Per-tab approval, drawn as a ✓ on the tab itself — the tab you judged is
    /// the honest place for it.
    pub approved: [bool; 3],
    /// The reviewer's report as the last tab. `None` = never reviewed. Boxed
    /// because it is by far the biggest thing in `Screen`.
    pub review: Option<Box<ReviewView>>,
    /// Marker on the Review tab label: the verdict, since there is no ✓ to show.
    pub review_mark: Span<'static>,
    /// Which `TABS` you can actually enter: the ones with something behind them.
    /// `←/→` and `↵` only ever visit these.
    pub live: Vec<usize>,
    /// Which `TABS` are drawn, in strip order — the journey, whether or not it
    /// has happened yet. Everything in `shown` and not in `live` is greyed: it
    /// says what is coming without pretending to be reachable.
    pub shown: Vec<usize>,
    /// Failure evidence + what to do about it. `None` = the run never failed.
    pub fail: Option<Vec<Line<'static>>>,
    /// The spec was revised after these patches were cut, so the Tests and Work
    /// tabs describe the previous feature. Say so on the tab rather than showing
    /// a diff that silently no longer matches the spec above it.
    pub superseded: bool,
    /// The action row of whatever is being asked, when something is.
    pub confirm: Option<(Ask, Buttons)>,
    /// Already applied to the working tree, so the next move is commit (or
    /// unstage), not stage. The hint bar and `next_step` read this; the stage box
    /// itself asks the run state, which is the one that cannot go stale.
    pub staged: bool,
}

impl CaseView {
    /// Where the current tab sits in the strip. `tab` is an index into `TABS`,
    /// which is not the same thing once a tab in the middle is missing.
    pub fn tab_pos(&self) -> usize {
        self.shown.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    /// Move `d` places along the strip, wrapping, skipping the tabs that are
    /// only drawn. A greyed tab must never be a destination: it looks like a
    /// control and answers to nothing.
    pub fn step(&mut self, d: isize) {
        let n = self.shown.len() as isize;
        let mut i = self.tab_pos() as isize;
        // `live` always holds Spec, so this cannot spin: at worst it comes home.
        for _ in 0..n {
            i = (i + d).rem_euclid(n);
            let t = self.shown[i as usize];
            if self.live.contains(&t) {
                self.tab = t;
                self.scroll.top();
                return;
            }
        }
    }
}

/// The one thing to do next, in a sentence, naming the key that does it.
pub fn next_step(
    g: &state::Gates,
    status: &Status,
    edited: bool,
    superseded: bool,
    ran: bool,
) -> Line<'static> {
    let key = |k: &str, rest: &str| {
        Line::from(vec![
            Span::styled(" ▸ ", Style::new().fg(Color::DarkGray)),
            Span::styled(k.to_string(), Style::new().fg(Color::Red).bold()),
            Span::styled(format!(" {rest}"), Style::new().fg(Color::Yellow)),
        ])
    };
    // Like `key`, but the hotkey is the highlighted letter inside the sentence
    // itself (btop-style) — the red `s` in "staged" is both the state and the key.
    let embed = |sentence: &str, k: char| -> Line<'static> {
        let yellow = Style::new().fg(Color::Yellow);
        let mut spans = vec![Span::styled(" ▸ ", Style::new().fg(Color::DarkGray))];
        match sentence.find(k) {
            Some(i) => {
                spans.push(Span::styled(sentence[..i].to_string(), yellow));
                spans.push(Span::styled(k.to_string(), Style::new().fg(Color::Red).bold()));
                spans.push(Span::styled(sentence[i + k.len_utf8()..].to_string(), yellow));
            }
            None => spans.push(Span::styled(sentence.to_string(), yellow)),
        }
        Line::from(spans)
    };
    match status {
        Status::Committed => Line::styled(
            " ✓ committed — Guv'nor does not push, sending it anywhere is yours",
            Style::new().fg(Color::Green),
        ),
        // A rejection is a decision, not a break: the advice below still applies.
        Status::Failed(why) if !why.starts_with("rejected_") => {
            key("←/→", "the run failed — the Failure tab has the evidence and the way out")
        }
        _ if edited => key("↵", "the spec changed since you approved it — approve it again"),
        _ if !g.spec.approved => key("↵", "approve this spec — it gates the run"),
        _ if !ran => {
            key("r", "run the lanes: test-writer → red gate → implementer → green gate → review")
        }
        _ if superseded => key("r", "the spec was revised after this run — re-run the lanes"),
        _ if !g.tests.approved => key("←/→", "read the Tests tab, then ↵ to judge it"),
        _ if !g.work.approved => key("←/→", "read the Work tab, then ↵ to judge it"),
        Status::Staged => embed("staged in your tree — look at it, then commit or unstage", 's'),
        _ => embed("stage it — every gate is green, into your working tree", 's'),
    }
}

impl App {

    pub fn build_case(&self, id: &str) -> Result<CaseView> {
        let dir = state::resolve_run_dir(&self.repo, id)?;
        let st = State::load(&dir)?;
        let spec = Spec::load(&dir.join("spec.json"));
        // Only the failure case needs lines: a readable spec is drawn as boxes.
        let spec_tab: Vec<Line> = match &spec {
            Ok(_) => Vec::new(),
            Err(e) => vec![Line::raw(format!("spec unreadable: {e:#}"))],
        };
        // Each gate tab carries its own proof: the diff, file by file, and the
        // evidence that makes it worth anything — red on base for the tests,
        // green with the implementation for the work. Both are rows you drop
        // open, so the tab opens as a list of what changed, not a wall of diff.
        let evidence = |lines: Option<String>| match lines {
            Some(t) => t.lines().map(failure_line).collect(),
            None => vec![Line::raw("  (none recorded)")],
        };
        let tests_diff = DiffView::build(
            &dir.join("tests.patch"),
            Line::styled(
                "red evidence — these tests failed on base, as required",
                Style::new().fg(Color::Red),
            ),
            evidence(Some(st.red_reason.clone()).filter(|t| !t.is_empty())),
        );
        // The review is NOT folded in here — it has its own tab, so this one is
        // only what you are being asked to approve: the code.
        let work_diff = DiffView::build(
            &dir.join("impl.patch"),
            Line::styled(
                "green evidence — the tests pass with this implementation",
                Style::new().fg(Color::Green),
            ),
            evidence(std::fs::read_to_string(dir.join("green.txt")).ok()),
        );

        let status = Line::from(vec![status_badge(&st.status), Span::raw(" ")]);
        // One hash of spec.json, checked against both pins below.
        let sha = spec_sha(&dir);
        // The patches on disk were derived from a spec that has since been
        // revised: their content is still evidence, but it describes the old
        // feature. `replan` already dropped the approvals; this is what tells
        // you why the ✓ went away.
        let superseded = spec_drifted(sha.as_deref(), &st.spec_sha_at_run);
        let fail_tab = build_fail_tab(&dir, &st);
        let gates_done =
            st.gates.spec.approved && st.gates.tests.approved && st.gates.work.approved;
        // Said out loud on the stage box when the verdict was not a clean
        // APPROVED — it refuses nothing, you approved the work gate having read
        // it, but a landing surface that doesn't mention it is hiding it.
        let flagged = std::fs::read_to_string(dir.join("review.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<crate::review::Review>(&raw).ok())
            .map(|r| r.verdict.verdict)
            .filter(|d| *d != crate::review::Decision::Approved);
        let mut review = self.build_review(&dir, &st);
        if let Some(r) = review.as_mut() {
            // The stage box lives at the foot of the Review tab. `Some` once
            // every gate is green (ready to land), `None` keeps it muted.
            r.stage = gates_done.then(|| StageView::build(&st.id, &st.status, flagged));
        }
        let review_mark = match &review {
            Some(r) => r.mark.clone(),
            None => Span::styled("· ", Style::new().fg(Color::DarkGray)),
        };
        let review = review.map(Box::new);
        // Drawn: the journey. Enterable: the parts of it that exist. A tab is
        // enterable exactly when the artifact behind it is on disk, so the strip
        // fills in as the run produces evidence and can never offer an empty page.
        let has = |f: &str| dir.join(f).is_file();
        let mut live = vec![0];
        let mut shown = vec![0, 1, 2, REVIEW_TAB];
        if has("tests.patch") {
            live.push(1);
        }
        if has("impl.patch") {
            live.push(2);
        }
        if review.is_some() {
            live.push(REVIEW_TAB);
        }
        if fail_tab.is_some() {
            live.push(FAIL_TAB);
            shown.push(FAIL_TAB);
        }
        let spec_edited =
            st.gates.spec.approved && spec_drifted(sha.as_deref(), &st.gates.spec.sha256);
        let next =
            next_step(&st.gates, &st.status, spec_edited, superseded, has("tests.patch"));
        let info = vec![Span::styled(st.title.clone(), Style::new().bold())];
        Ok(CaseView {
            id: st.id,
            dir,
            tab: 0,
            scroll: Scroll::default(),
            note: None,
            feedback: None,
            info: Line::from(info),
            status,
            next,
            live,
            shown,
            superseded,
            spec_lines: spec_tab,
            diffs: [tests_diff, work_diff],
            spec: spec.ok(),
            panels: SpecPanels::default(),
            approved: [st.gates.spec.approved, st.gates.tests.approved, st.gates.work.approved],
            review,
            review_mark,
            fail: fail_tab,
            confirm: None,
            staged: st.status == Status::Staged,
        })
    }

    pub fn render_case(&self, f: &mut Frame, area: Rect) {
        let Screen::Case(v) = &self.screen else { return };
        let has_note = v.note.is_some();
        let [top_a, body_a, note_a] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(if has_note { 3 } else { 0 }),
        ])
        .areas(area);
        // tabs box · run info. Approval lives on the tab as a ✓. The Review tab
        // shows the verdict instead: there is nothing to approve there.
        let labels: Vec<Line> = v
            .shown
            .iter()
            .map(|&i| {
                // Not reachable yet: one dim colour for mark and label both, so
                // it reads as "later", not as a control that ignores you.
                if !v.live.contains(&i) {
                    let dim = Style::new().fg(Color::DarkGray);
                    return Line::from(vec![Span::styled("· ", dim), Span::styled(TABS[i], dim)]);
                }
                let mark = match (v.approved.get(i), i) {
                    (Some(true), _) => Span::styled("✓ ", Style::new().fg(Color::Green).bold()),
                    (Some(false), 1 | 2) if v.superseded => {
                        Span::styled("⚠ ", Style::new().fg(Color::Yellow).bold())
                    }
                    (Some(false), _) => Span::styled("· ", Style::new().fg(Color::DarkGray)),
                    (None, REVIEW_TAB) => v.review_mark.clone(),
                    _ => Span::styled("✖ ", Style::new().fg(Color::Red).bold()),
                };
                Line::from(vec![mark, Span::raw(TABS[i])])
            })
            .collect();
        let tabs_w = labels.iter().map(|l| l.width() as u16 + 3).sum::<u16>() + 1;
        let [tabs_a, info_a] =
            Layout::horizontal([Constraint::Length(tabs_w), Constraint::Min(0)]).areas(top_a);
        f.render_widget(
            Tabs::new(labels)
                .select(v.tab_pos())
                .highlight_style(Style::new().bg(Color::White).fg(Color::Black).bold())
                .block(boxed("", Style::new())),
            tabs_a,
        );
        // One row, three things: the run's name from the left, the status
        // hard right so it never moves when the title does, and what to do next
        // in the gap between them. Two passes plus a sub-rect is cheaper than a
        // layout split and lets the message wrap into the spare row below rather
        // than being clipped on a narrow terminal.
        let mut info = v.info.clone();
        info.spans.insert(0, Span::raw(" "));
        let name_w = info.width() as u16;
        f.render_widget(Paragraph::new(vec![Line::raw(""), info]), info_a);
        f.render_widget(
            Paragraph::new(vec![Line::raw(""), v.status.clone()]).right_aligned(),
            info_a,
        );
        let gap = Rect {
            x: info_a.x + name_w + 3,
            y: info_a.y + 1,
            width: info_a
                .width
                .saturating_sub(name_w + 3 + v.status.width() as u16 + 1),
            height: info_a.height.saturating_sub(1),
        };
        f.render_widget(Paragraph::new(v.next.clone()).wrap(Wrap { trim: true }), gap);
        match (v.tab, &v.review) {
            // The report brings its own boxes; a wrapper round them would be a
            // third border deep for nothing.
            (REVIEW_TAB, Some(r)) => render_review_tab(f, body_a, r),
            (FAIL_TAB, _) => {
                let block = boxed("Failure", Style::new().fg(Color::Red).bold())
                    .border_style(Style::new().fg(Color::Red))
                    .title_style(Style::new().fg(Color::Red));
                let inner = block.inner(body_a);
                let lines = v.fail.clone().unwrap_or_default();
                let body = Paragraph::new(lines).wrap(Wrap { trim: false });
                let off = v.scroll.fit(body.line_count(inner.width), inner.height);
                f.render_widget(body.scroll((off, 0)).block(block), body_a);
            }
            (REVIEW_TAB, None) => f.render_widget(
                Paragraph::new(Line::styled(
                    " not reviewed yet — the reviewer runs at the end of a run",
                    Style::new().fg(Color::DarkGray),
                ))
                .block(boxed("Review", Style::new().bold())),
                body_a,
            ),
            _ => {
                let stale = v.superseded && v.tab > 0;
                let title = match (v.approved[v.tab], stale) {
                    (_, true) => format!("{} — from a superseded spec, re-run (r)", TABS[v.tab]),
                    (true, _) => format!("{} — approved", TABS[v.tab]),
                    (false, _) => format!("{} — ↵ to judge", TABS[v.tab]),
                };
                let style = if stale {
                    Style::new().fg(Color::Yellow).bold()
                } else {
                    Style::new().bold()
                };
                let block = boxed(&title, style);
                // The diff tabs carry their own keys: the strip's hints are
                // about the run, these are about the list in front of you.
                let block = match v.tab {
                    1 | 2 => block.title_bottom(hint_line(&[
                        ("↑↓", "file"),
                        ("space", "open / close"),
                        ("↵", "judge"),
                    ])),
                    _ => block,
                };
                let inner = block.inner(body_a);
                f.render_widget(block, body_a);
                // The spec is sections, not prose: always boxes, one column or
                // two depending on the room. Tests and Work are file lists.
                match (v.tab, &v.spec) {
                    (0, Some(sp)) => render_spec_panels(f, inner, sp, &v.panels),
                    (1 | 2, _) => render_diff(f, inner, &v.diffs[v.tab - 1]),
                    _ => {
                        let body =
                            Paragraph::new(v.spec_lines.clone()).wrap(Wrap { trim: false });
                        let off = v.scroll.fit(body.line_count(inner.width), inner.height);
                        f.render_widget(body.scroll((off, 0)), inner);
                    }
                }
            }
        }
        if let Some((ask, buttons)) = &v.confirm {
            // The gate ask needs no words — the tab you're on and the two verbs
            // say it. The re-run ask does: it is not an undo.
            let msg: &[&str] = match ask {
                Ask::Gate => &[],
                Ask::Rerun => &[
                    "three fresh lanes: test-writer, implementer, reviewer.",
                    "The patches and the review you have now are replaced.",
                ],
            };
            let (w, title) = match ask {
                Ask::Gate => (44, format!("{} gate", TABS[v.tab])),
                Ask::Rerun => (60, "re-run".to_string()),
            };
            let words = if msg.is_empty() { 0 } else { msg.len() as u16 + 1 };
            let [pc] = Layout::horizontal([Constraint::Length(w.min(area.width))])
                .flex(Flex::Center)
                .areas(area);
            let [popup] = Layout::vertical([Constraint::Length(5 + words)])
                .flex(Flex::Center)
                .areas(pc);
            let block =
                modal(&title, &[("←/→", "choose"), ("↵", "confirm"), ("esc", "cancel")]);
            let inner = block.inner(popup);
            f.render_widget(Clear, popup);
            f.render_widget(block, popup);
            let [msg_a, btn_a] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(inner);
            if !msg.is_empty() {
                let lines: Vec<Line> = std::iter::once(Line::raw(""))
                    .chain(msg.iter().map(|m| Line::raw(format!(" {m}"))))
                    .collect();
                f.render_widget(Paragraph::new(lines), msg_a);
            }
            buttons.render(f, btn_a, true);
        }
        if let Some(fb) = &v.feedback {
            let [pc] = Layout::horizontal([Constraint::Percentage(70)]).flex(Flex::Center).areas(area);
            let [popup] = Layout::vertical([Constraint::Length(12)]).flex(Flex::Center).areas(pc);
            f.render_widget(Clear, popup);
            let block = modal(
                "iterate — feedback for the planner",
                &[("tab", "text / actions"), ("⇧↵", "newline"), ("↵", "send"), ("esc", "cancel")],
            );
            let inner = block.inner(popup);
            f.render_widget(block, popup);
            let [text_a, btn_a] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(inner);
            fb.buttons.render(f, btn_a, fb.on_buttons);
            render_textarea(f, text_a, &fb.text, !fb.on_buttons);
        }
        if let Some(note) = &v.note {
            // Scrolled like every other one-line field: without this the text
            // stops at the box edge while the cursor keeps walking right, so
            // past ~70 characters you are typing blind.
            let (xoff, cx) = hscroll(note.cursor, note_a.width.saturating_sub(2) as usize);
            f.render_widget(
                Paragraph::new(note.value.as_str()).scroll((0, xoff)).block(
                    boxed("rejection note (↵ confirm · esc cancel)", Style::new())
                        .border_style(Style::new().fg(Color::Red))
                        .title_style(Style::new().fg(Color::Red)),
                ),
                note_a,
            );
            f.set_cursor_position(Position::new(note_a.x + 1 + cx, note_a.y + 1));
        }
    }

    pub fn render_landed(&self, f: &mut Frame, area: Rect) {
        let Screen::Landed { title, msg } = &self.screen else { return };
        let lines = vec![
            Line::styled(" done ✓", Style::new().fg(Color::Green).bold()),
            Line::raw(""),
            Line::raw(format!(" {msg}")),
            Line::raw(""),
            Line::styled(
                " Guv'nor does not push. Sending it anywhere is your call.",
                Style::new().fg(Color::DarkGray),
            ),
        ];
        f.render_widget(
            Paragraph::new(lines).block(
                boxed(title, Style::new().fg(Color::Green).bold())
                    .border_style(Style::new().fg(Color::Green))
                    .title_style(Style::new().fg(Color::Green)),
            ),
            area,
        );
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hole: an approval has to die with the thing it approved. `replan`
    /// resets the tests/work gates (engine side); this is the half that tells
    /// you why, so a diff from a superseded spec can't be read as current.
    #[test]
    fn a_revised_spec_marks_its_old_patches_superseded() {
        let dir = std::env::temp_dir().join(format!("guvnor-stale-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"SPEC-V1").unwrap();
        let stale = |st: &State| spec_drifted(spec_sha(&dir).as_deref(), &st.spec_sha_at_run);

        let mut st = State::new("20260101T000000-x", "t");
        assert!(!stale(&st), "nothing run yet is not stale");
        // a run pins the spec its patches came from
        st.spec_sha_at_run = digest::sha256_hex(b"SPEC-V1");
        assert!(!stale(&st));
        // replan rewrites spec.json — the patches now describe the old feature
        std::fs::write(dir.join("spec.json"), b"SPEC-V2").unwrap();
        assert!(stale(&st), "a revised spec must not leave its diffs looking current");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn view(live: Vec<usize>, shown: Vec<usize>) -> CaseView {
        CaseView {
            id: "x".into(),
            dir: std::path::PathBuf::from("/nonexistent"),
            tab: 0,
            scroll: Scroll::default(),
            note: None,
            feedback: None,
            info: Line::raw(""),
            status: Line::raw(""),
            next: Line::raw(""),
            spec_lines: vec![],
            diffs: Default::default(),
            spec: None,
            panels: SpecPanels::default(),
            approved: [true, true, true],
            review: None,
            review_mark: Span::raw(""),
            live,
            shown,
            fail: None,
            superseded: false,
            confirm: None,
            staged: false,
        }
    }

    /// `r` on a run that already has patches is a re-run: it bins them and pays
    /// for three more lanes, so it asks first, with `cancel` preselected. The
    /// first run has nothing to lose and fires straight away.
    #[test]
    fn a_rerun_asks_first_but_the_first_run_does_not() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::KeyCode;
        use ratatui::Terminal;
        let asking = |app: &App| match &app.screen {
            Screen::Case(v) => v.confirm.is_some(),
            _ => unreachable!(),
        };
        // nothing run yet: only the Spec tab is live, so `r` just goes
        let mut app = App::for_test();
        app.screen = Screen::Case(Box::new(view(vec![0], vec![0, 1, 2])));
        assert!(
            matches!(app.handle_key(&press(KeyCode::Char('r'))), Some(Go::Run(_))),
            "the first run has nothing to confirm"
        );

        // patches exist: `r` opens the ask instead of firing
        let mut app = App::for_test();
        app.screen = Screen::Case(Box::new(view(vec![0, 1, 2], vec![0, 1, 2])));
        assert!(app.handle_key(&press(KeyCode::Char('r'))).is_none(), "no run yet");
        assert!(asking(&app), "it asks");
        // it says what it costs, and the safe answer is the preselected one
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| app.render_case(f, Rect::new(0, 0, 80, 24))).unwrap();
        let screen = screen_text(t.backend().buffer());
        assert!(screen.contains("are replaced"), "the ask says what it costs: {screen:?}");
        // ↵ on the preselected button cancels; the run does not start
        assert!(app.handle_key(&press(KeyCode::Enter)).is_none());
        assert!(!asking(&app), "cancel closes it");

        // and choosing `re-run` is what fires the job
        app.handle_key(&press(KeyCode::Char('r')));
        app.handle_key(&press(KeyCode::Right));
        assert!(
            matches!(app.handle_key(&press(KeyCode::Enter)), Some(Go::Run(_))),
            "→ ↵ is the deliberate answer"
        );
        assert!(!asking(&app), "and the modal closes behind it");
    }

    /// Landing is the stage box at the foot of the Review tab — `s` from any
    /// tab jumps there and focuses it, and the box renders (a full file list +
    /// buttons) without panicking, roomy or cramped.
    #[test]
    fn s_jumps_to_the_stage_box_on_review() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::KeyCode;
        use ratatui::Terminal;
        let live = vec![0, 1, 2, REVIEW_TAB];
        let mut app = App::for_test();
        let mut v = view(live.clone(), live);
        v.review = Some(Box::new(ReviewView::stub(
            1,
            Some(StageView::build("nope", &Status::Reviewed, None)),
        )));
        v.tab = 2; // on the Work tab: s must reach the box from anywhere
        app.screen = Screen::Case(Box::new(v));

        app.handle_key(&press(KeyCode::Char('s')));
        let Screen::Case(v) = &app.screen else { unreachable!() };
        assert_eq!(v.tab, REVIEW_TAB, "s jumps to the Review tab");
        assert!(
            matches!(v.review.as_deref(), Some(r) if r.focus == ReviewFocus::Stage),
            "and focuses the stage box"
        );

        // the box draws over the Review tab at a roomy and a cramped size —
        // the height math must not panic on a small terminal
        for (w, h) in [(100u16, 30u16), (60, 16)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
        }
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| app.render_case(f, Rect::new(0, 0, 100, 30))).unwrap();
        let screen = screen_text(t.backend().buffer());
        assert!(screen.contains("stage —"), "the box titles itself: {screen:?}");
        assert!(screen.contains("stage"), "and offers the stage action: {screen:?}");
    }

    /// The strip draws the whole journey, and stepping must visit only the parts
    /// of it that have happened — a greyed tab must never be a destination: it
    /// looks like a control and answers to nothing.
    #[test]
    fn stepping_the_strip_only_visits_live_tabs() {
        // failed AND fully approved: Failure is the only conditional tab
        // (landing is the `s` box on the Review tab, not a tab)
        let all = vec![0, 1, 2, REVIEW_TAB, FAIL_TAB];
        let mut v = view(all.clone(), all);
        // forward through every tab and round to the start
        let seen: Vec<usize> = (0..5)
            .map(|_| {
                v.step(1);
                v.tab
            })
            .collect();
        assert_eq!(seen, [1, 2, REVIEW_TAB, FAIL_TAB, 0]);
        // backwards wraps the other way
        v.step(-1);
        assert_eq!(v.tab, FAIL_TAB);
        assert_eq!(v.tab_pos(), 4, "the strip position is not the TABS index");

        // a planned run: the whole journey is drawn, only the spec is enterable
        let mut v = view(vec![0], vec![0, 1, 2, REVIEW_TAB]);
        for d in [1, -1, 1, 1] {
            v.step(d);
            assert_eq!(v.tab, 0, "greyed tabs are not destinations");
        }

        // mid-run: tests exist, work does not — stepping jumps the gap in both
        // directions rather than opening an empty page
        let mut v = view(vec![0, 1], vec![0, 1, 2, REVIEW_TAB]);
        v.step(1);
        assert_eq!(v.tab, 1);
        v.step(1);
        assert_eq!(v.tab, 0, "Work and Review are drawn but not reachable yet");
        v.step(-1);
        assert_eq!(v.tab, 1);
        // ...and the strip position still tracks what is drawn, not what is live
        assert_eq!(v.tab_pos(), 1);
    }

    /// The strip draws the whole journey, so it must say which parts of it are
    /// reachable — a dim label is a promise, a bright one is a control.
    #[test]
    fn the_strip_draws_the_whole_journey_and_greys_what_has_not_happened() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = App::for_test();
        // a freshly planned run: nothing but a spec
        app.screen = Screen::Case(Box::new(view(vec![0], vec![0, 1, 2, REVIEW_TAB])));
        let (w, h) = (100, 20);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
        let buf = t.backend().buffer().clone();
        let cells: Vec<String> = (0..w).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let row = cells.concat();
        // the whole journey is named from the start — that is the map
        for label in ["Spec", "Tests", "Work", "Review"] {
            assert!(row.contains(label), "{label} missing from the strip: {row}");
        }
        // Failure is not a stage of the journey, so it is not promised; landing
        // is the `s` box on the Review tab, not a tab of its own
        assert!(!row.contains("Failure"), "a run that hasn't failed must not offer it");
        assert!(!row.contains("Stage"), "landing is the s box on Review, not a tab");
        // by cell, not by byte: the row is mostly multi-byte glyphs
        let fg_at = |needle: &str| {
            let x = (0..w).find(|&x| cells[x as usize..].concat().starts_with(needle)).unwrap();
            buf[(x, 1)].style().fg
        };
        assert_eq!(fg_at("Tests"), Some(Color::DarkGray), "unreachable tabs must read as later");
        assert_eq!(fg_at("Work"), Some(Color::DarkGray));
        assert_ne!(fg_at("Spec"), Some(Color::DarkGray), "the tab you are on is not greyed");
    }

    /// The status goes hard right, the run's name stays left, and the
    /// message sits in the gap between them. A chip whose position depends on
    /// the length of the title is a chip you have to hunt for.
    #[test]
    fn name_left_message_in_the_gap_status_hard_right() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = App::for_test();
        let mut v = view(vec![0], vec![0, 1, 2, REVIEW_TAB]);
        v.info = Line::from(Span::raw("more math functions"));
        v.status = Line::from(vec![status_badge(&state::Status::Reviewed), Span::raw(" ")]);
        v.next = Line::from(vec![
            Span::raw(" ▸ "),
            Span::raw("c"),
            Span::raw(" every gate is green"),
        ]);
        app.screen = Screen::Case(Box::new(v));
        let (w, h) = (160, 20);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render_case(f, Rect::new(0, 0, w, h))).unwrap();
        let buf = t.backend().buffer().clone();
        let cells: Vec<String> = (0..w).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let x_of = |needle: &str| {
            (0..w).find(|&x| cells[x as usize..].concat().starts_with(needle)).unwrap()
        };
        // the chip's fill reaches the last usable column, whatever the title is
        assert_eq!(buf[(w - 2, 1)].style().bg, Some(Color::Cyan), "status is not flush right");
        // all three on one row, in that order
        assert!(x_of("more math functions") < x_of("every gate is green"), "{}", cells.concat());
        assert!(x_of("every gate is green") < x_of("reviewed"), "{}", cells.concat());
    }

    /// Whatever state a run is in, the screen says the next move and names the
    /// key.
    #[test]
    fn the_next_move_is_always_on_screen() {
        let text = line_text;
        let key = |l: &Line| l.spans[1].content.to_string();
        let gates = |s, t, w| {
            let mut g = state::Gates::default();
            g.spec.approved = s;
            g.tests.approved = t;
            g.work.approved = w;
            g
        };
        let go = state::Status::SpecApproved;

        // unapproved: approving is the only move, and it is ↵
        let l = next_step(&gates(false, false, false), &go, false, false, false);
        assert_eq!(key(&l), "↵");
        assert!(text(&l).contains("approve"), "{}", text(&l));

        // approved and never run — THE reported gap
        let l = next_step(&gates(true, false, false), &go, false, false, false);
        assert_eq!(key(&l), "r");
        assert!(text(&l).contains("run the lanes"), "{}", text(&l));

        // run done: judge the tests, then the work, then land it
        let l = next_step(&gates(true, false, false), &Status::Reviewed, false, false, true);
        assert!(text(&l).contains("Tests"), "{}", text(&l));
        let l = next_step(&gates(true, true, false), &Status::Reviewed, false, false, true);
        assert!(text(&l).contains("Work"), "{}", text(&l));
        let l = next_step(&gates(true, true, true), &Status::Reviewed, false, false, true);
        assert!(text(&l).contains("stage"), "{}", text(&l));
        assert!(
            l.spans.iter().any(|s| s.content == "s" && s.style.fg == Some(Color::Red)),
            "the s hotkey is red, embedded in the sentence: {}",
            text(&l)
        );
        let l = next_step(&gates(true, true, true), &Status::Staged, false, false, true);
        assert!(text(&l).contains("staged in your tree"), "{}", text(&l));
        assert!(text(&l).contains("commit"), "{}", text(&l));
        assert!(
            l.spans.iter().any(|s| s.content == "s" && s.style.fg == Some(Color::Red)),
            "the s in 'staged' is the red hotkey: {}",
            text(&l)
        );

        // an edited spec outranks everything short of a break: the approval on
        // record is for different words
        let l = next_step(&gates(true, true, true), &Status::Reviewed, true, false, true);
        assert!(text(&l).contains("changed since you approved"), "{}", text(&l));
        // ...and a break outranks that
        let broke = Status::Failed("vacuous_tests".into());
        let l = next_step(&gates(true, true, true), &broke, true, false, true);
        assert!(text(&l).contains("Failure tab"), "{}", text(&l));
        // a rejection is a decision, not a break — it must not claim a Failure tab
        let no = Status::Failed("rejected_work".into());
        let l = next_step(&gates(false, false, false), &no, false, false, false);
        assert!(text(&l).contains("approve"), "{}", text(&l));
        // done is done: no key to press, nothing left to do
        let l = next_step(&gates(true, true, true), &Status::Committed, false, false, true);
        assert!(text(&l).contains("committed"), "{}", text(&l));
        assert!(!text(&l).contains('▸'), "nothing to press: {}", text(&l));
    }

    #[test]
    fn tab_maps_to_its_gate() {
        // a wrong mapping here silently approves the wrong gate — assert all
        // three, and that the tabs without one are the reports (landing is not a
        // tab: it is the `s` box on Review)
        assert_eq!(TABS.len(), 5);
        assert_eq!(tab_gate(0).as_str(), "spec");
        assert_eq!(tab_gate(1).as_str(), "tests");
        assert_eq!(tab_gate(2).as_str(), "work");
        assert_eq!(TABS[REVIEW_TAB], "Review");
        assert_eq!(TABS[FAIL_TAB], "Failure");
        // the gate array is indexed by tab: the rest must sit past its end
        assert_eq!(REVIEW_TAB, 3);
        assert_eq!(FAIL_TAB, TABS.len() - 1);
    }

}
