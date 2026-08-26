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
    Clear, Paragraph, Wrap,
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
pub fn spec_sha(dir: &std::path::Path) -> Option<String> {
    std::fs::read(dir.join("spec.json")).ok().map(|b| digest::sha256_hex(&b))
}

/// Whether the spec on disk no longer hashes to `pin` — the sha recorded when
/// a run cut its patches, or when the spec gate was approved. An empty pin
/// means nothing was recorded, so nothing has drifted.
pub fn spec_drifted(sha: Option<&str>, pin: &str) -> bool {
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
    /// Each shown tab's on-screen cell, from the last render. `shown[k]` is
    /// the tab `tab_cells[k]` draws; a click reuses this, so it can't target
    /// something different from what's drawn. Empty until the first frame.
    pub tab_cells: Vec<Rect>,
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

    /// Jump straight to tab `t`: a click's counterpart to `step`'s relative
    /// move. A greyed tab is a no-op, same as the keyboard.
    pub fn goto(&mut self, t: usize) {
        if self.live.contains(&t) {
            self.tab = t;
            self.scroll.top();
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
            tab_cells: Vec::new(),
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

    pub fn render_case(&mut self, f: &mut Frame, area: Rect) {
        let Screen::Case(v) = &mut self.screen else { return };
        let has_note = v.note.is_some();
        // 2 rows, not 3: the strip's own bottom border is gone. The body
        // box's top border, stitched by `tab_strip` below, is now the only
        // line between them.
        let [top_a, body_a, note_a] = Layout::vertical([
            Constraint::Length(2),
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
        let tabs_w = tab_strip_width(&labels);
        let [tabs_a, info_a] =
            Layout::horizontal([Constraint::Length(tabs_w), Constraint::Min(0)]).areas(top_a);
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
            (REVIEW_TAB, Some(r)) => {
                // Outer box, untitled, like Spec/Tests/Work: gives the seam
                // a title-free row to land on. The report's own boxes
                // (verdict, findings, cost) sit inset below with their own
                // titles.
                let block = boxed("", Style::new());
                let inner = block.inner(body_a);
                f.render_widget(block, body_a);
                render_review_tab(f, inner, r);
            }
            (FAIL_TAB, _) => {
                // No title: this border is the row the strip shares. The
                // tab's own red ✖ mark already says "broken".
                let block = boxed("", Style::new()).border_style(Style::new().fg(Color::Red));
                let inner = block.inner(body_a);
                let lines = v.fail.clone().unwrap_or_default();
                let body = Paragraph::new(lines).wrap(Wrap { trim: false });
                let off = v.scroll.fit(body.line_count(inner.width), inner.height);
                f.render_widget(body.scroll((off, 0)).block(block), body_a);
            }
            // No title: same shared-seam row as every other body box.
            (REVIEW_TAB, None) => f.render_widget(
                Paragraph::new(Line::styled(
                    " not reviewed yet — the reviewer runs at the end of a run",
                    Style::new().fg(Color::DarkGray),
                ))
                .block(boxed("", Style::new())),
                body_a,
            ),
            _ => {
                let stale = v.superseded && v.tab > 0;
                // No title: it used to spell out "approved" / "↵ to judge" /
                // "from a superseded spec, re-run (r)", but that text sits on
                // the row the strip's seam shares, and collides with a wall.
                // The tab's own mark (✓ ✖ · ⚠) says the same thing, and
                // `next_step` above names the key.
                let block = boxed("", Style::new());
                let block = if stale {
                    block.border_style(Style::new().fg(Color::Yellow))
                } else {
                    block
                };
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
        // Drawn last, so its seam overwrite lands on top of the content
        // box's own top border. Reads as one frame with a notch, not two
        // stacked. Cached for the next click's hit-test.
        let tab_pos = v.tab_pos();
        v.tab_cells = tab_strip(f, tabs_a, &labels, tab_pos);
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
