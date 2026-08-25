//! Engine: the orchestration ops behind the CLI verbs (and later the TUI /
//! server). Long ops (plan, run) report via `Progress` over an mpsc sender
//! and return an exit code; short ops (set_gate, commit) return a message.

use crate::config::{self, Config};
use crate::spec::{self, Spec};
use crate::state::{self, State, Status};
use crate::{digest, events, harness, lane, review, worktree};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub enum Progress {
    /// A plan op created this run id (TUI opens the spec screen with it).
    RunCreated { id: String },
    /// Human-readable step announcement ("[1/5] test-writer lane ...").
    Stage(String),
    /// Raw stream-json stdout line from a lane (verbose views).
    LaneLine { lane: String, line: String },
    /// Deterministic gate outcome; `ok` means the gate is satisfied
    /// (red gate is satisfied when tests FAIL on base).
    GateResult { gate: String, ok: bool, detail: String },
    /// Terminal success; message includes next-step hints.
    Done(String),
    /// Terminal failure recorded in state.json; artifacts kept.
    Failed { why: String, detail: String },
}

fn lane_sink(tx: &Sender<Progress>, lane: &str) -> Option<Box<dyn FnMut(String) + Send>> {
    let tx = tx.clone();
    let lane = lane.to_string();
    Some(Box::new(move |line| {
        let _ = tx.send(Progress::LaneLine { lane: lane.clone(), line });
    }))
}

fn lane_report(name: &str, r: &lane::LaneResult, tx: &Sender<Progress>) {
    let _ = tx.send(Progress::Stage(format!(
        "      {name}: {}→{} tok · ${:.4} · {}s",
        r.tokens_in, r.tokens_out, r.cost_usd, r.duration_secs
    )));
    if r.exit_code != Some(0) && !r.timed_out {
        // Keep going with whatever we captured; caller decides via gates.
        let _ = tx.send(Progress::Stage(format!(
            "lane '{name}' exited {:?}; stderr tail: {}",
            r.exit_code, r.stderr_tail
        )));
    }
}

fn usage_json(r: &lane::LaneResult) -> serde_json::Value {
    json!({"tokens_in": r.tokens_in, "tokens_out": r.tokens_out, "cost_usd": r.cost_usd})
}

/// Terminal failure: record the machine reason in state.json, log it, tell the
/// caller. Artifacts and worktrees are deliberately left for inspection.
fn fail(
    run_dir: &Path,
    st: &mut State,
    log: &events::EventLog,
    tx: &Sender<Progress>,
    why: &str,
    detail: String,
) -> Result<i32> {
    st.status = Status::Failed(why.to_string());
    st.save(run_dir)?;
    log.append("run_failed", json!({"why": why, "detail": detail}))?;
    let _ = tx.send(Progress::Failed {
        why: why.to_string(),
        detail: format!(
            "{detail}\nartifacts kept in {} and worktrees for inspection",
            run_dir.display()
        ),
    });
    Ok(2)
}

fn merge_json(into: &mut serde_json::Value, from: serde_json::Value) {
    if let (Some(a), Some(b)) = (into.as_object_mut(), from.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}

/// Why two patches can't both land, in terms the human can act on in the spec.
/// Put the previous run's implementation into the implementer's tree, so a
/// re-run after a spec revision adapts it instead of writing it again from
/// nothing. Returns whether it landed; every reason it might not is a reason to
/// start cold, never to fail.
///
/// ponytail: automatic, no flag. A spec change big enough that the old
/// implementation actively misleads costs one rework round to discover — add a
/// cold-start escape hatch only if that starts happening.
fn seed_impl(wt_impl: &Path, run_dir: &Path, tests_patch: &str) -> bool {
    let Ok(prev) = std::fs::read_to_string(run_dir.join("impl.patch")) else { return false };
    if prev.trim().is_empty() {
        return false;
    }
    // The new tests may own files the old implementation created. Seeding would
    // make the two patches non-composable and fail as if the implementer had
    // misbehaved, which is a lie about what happened.
    if !worktree::overlapping_paths(tests_patch, &prev).is_empty() {
        return false;
    }
    // Base moved under it, or the patch is stale: cold pass it is.
    worktree::apply_patch(wt_impl, &prev).is_ok()
}

/// A lane that edited nothing. The tree is the evidence, but only the model's
/// own last words say WHY — a refusal, a misread spec, or a fence it couldn't
/// work around. The lane transcript on disk is the only other copy and nothing
/// reads it, so the failure carries the words.
fn noop_detail(res: &lane::LaneResult) -> String {
    let mut d = String::from("no edits reached the tree — the lane narrated instead of working");
    if res.denials > 0 {
        d.push_str(&format!(
            "\n{} hook denial(s): it tried to touch something it is fenced out of",
            res.denials
        ));
    }
    let said = res.result_text.trim();
    if !said.is_empty() {
        d.push_str("\n\nwhat the lane said:\n");
        d.push_str(said);
    }
    d
}

/// The usual cause: the spec's Files list names test files, so the implementer
/// creates them too and both patches try to add the same file.
fn overlap_detail(overlap: &[String]) -> String {
    format!(
        "the implementer wrote files the test-writer's patch already owns, so the two \
         patches cannot both apply:\n  {}\n\nFix in the spec: its Files list should not \
         ask the implementer for test files — drop those paths from Files, or state that \
         tests are written by the independent test lane.",
        overlap.join("\n  ")
    )
}

/// Extract + validate the spec JSON a planner lane returned.
fn parse_planner_spec(result_text: &str, default_verification: &str) -> Result<Spec> {
    let json_str =
        spec::extract_json_object(result_text).context("planner returned no JSON spec")?;
    let mut sp: Spec = serde_json::from_str(json_str).context("planner spec JSON invalid")?;
    if sp.verification.trim().is_empty() {
        sp.verification = default_verification.into();
    }
    sp.validate()?;
    Ok(sp)
}

/// Dep-free UUID v4 from /dev/urandom (guvnor is unix-only). Used as a stable
/// Claude session id so spec iterations resume one planner session.
fn new_session_id() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// Warn when the reviewer cannot decorrelate from the worker (same model). A
/// tier split (e.g. opus reviewing sonnet) is guvnor's only decorrelation in a
/// single-vendor setup; if both seats are the same model the review is the
/// author grading its own homework.
fn decorrelation_warning(cfg: &Config) -> Option<String> {
    if cfg.claude.model_reviewer == cfg.claude.model_worker {
        Some(format!(
            "⚠ reviewer and worker are the same model ({}) — the review is the author grading its own work; set a stronger [claude] model_reviewer",
            cfg.claude.model_worker
        ))
    } else {
        None
    }
}

/// Run a planner/replanner lane, report cost, log usage, and return its result
/// text. Shared by `plan` and `replan`. Cancellation/timeout are hard errors.
fn planner_lane(
    cfg: &Config,
    repo: &Path,
    run_dir: &Path,
    session: lane::Session,
    prompt: String,
    tx: &Sender<Progress>,
) -> Result<String> {
    let result = lane::run(lane::LaneSpec {
        cwd: repo,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_planner,
        prompt,
        allowed_tools: "Read,Glob,Grep",
        timeout: Duration::from_secs(cfg.limits.lane_timeout_secs),
        transcript: run_dir.join("lanes-planner.ndjson"),
        line_sink: lane_sink(tx, "planner"),
        session,
    })?;
    lane_report("planner", &result, tx);
    events::EventLog::new(run_dir).append("lane_planner", usage_json(&result))?;
    if result.cancelled {
        bail!("planner cancelled");
    }
    if result.timed_out {
        bail!("planner timed out");
    }
    Ok(result.result_text)
}

pub fn plan(title: &str, context: &str, tx: &Sender<Progress>) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let id = format!(
        "{}-{}",
        events::now_iso().replace([':', '-'], "").trim_end_matches('Z'),
        state::slugify(title, 24)
    );
    let run_dir = state::runs_root(&repo).join(&id);
    std::fs::create_dir_all(&run_dir)?;
    let log = events::EventLog::new(&run_dir);
    log.append("plan_started", json!({"title": title}))?;

    let _ = tx.send(Progress::Stage(format!(
        "planning '{title}' with {} ...",
        cfg.claude.model_planner
    )));
    // Open a persistent planner session so spec iterations can resume it.
    let session_id = new_session_id()?;
    let text = planner_lane(
        &cfg,
        &repo,
        &run_dir,
        lane::Session::Create(session_id.clone()),
        lane::planner_prompt(title, context, &cfg.commands.test),
        tx,
    )?;
    let sp = parse_planner_spec(&text, &cfg.commands.test)?;
    std::fs::write(run_dir.join("spec.json"), serde_json::to_string_pretty(&sp)?)?;
    let mut st = State::new(&id, title);
    st.planner_session_id = session_id;
    st.save(&run_dir)?;
    log.append("plan_drafted", json!({"spec_files": sp.files}))?;
    let _ = tx.send(Progress::RunCreated { id: id.clone() });

    let _ = tx.send(Progress::Done(format!(
        "spec draft: {}\nread it, edit it, then:  guvnor approve {id} --gate spec",
        run_dir.join("spec.json").display()
    )));
    Ok(0)
}

/// Revise an existing spec with human feedback. The planner sees the current
/// spec + feedback; the result replaces spec.json and invalidates any prior
/// spec approval by construction.
pub fn replan(id: &str, feedback: &str, tx: &Sender<Progress>) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    let prev = std::fs::read_to_string(run_dir.join("spec.json"))?;
    let log = events::EventLog::new(&run_dir);
    log.append("replan_started", json!({"feedback": feedback}))?;

    let _ = tx.send(Progress::Stage(format!(
        "revising spec for '{}' with {} ...",
        st.title, cfg.claude.model_planner
    )));
    // Resume the open planner session for a cheap delta (feedback only); if
    // there is no session yet, open one with the full cold replan prompt.
    let resuming = !st.planner_session_id.is_empty();
    let (session, prompt) = if resuming {
        (
            lane::Session::Resume(st.planner_session_id.clone()),
            lane::replan_feedback_prompt(feedback, &cfg.commands.test),
        )
    } else {
        let sid = new_session_id()?;
        st.planner_session_id = sid.clone();
        (
            lane::Session::Create(sid),
            lane::replanner_prompt(&st.title, &prev, feedback, &cfg.commands.test),
        )
    };
    let text = planner_lane(&cfg, &repo, &run_dir, session, prompt, tx)?;
    let sp = match parse_planner_spec(&text, &cfg.commands.test) {
        Ok(sp) => sp,
        // A resumed session that yields no spec is likely stale/GC'd — retry
        // once with a fresh session and the full cold replan prompt.
        Err(_) if resuming => {
            let _ = tx.send(Progress::Stage(
                "resume produced no spec; retrying with a fresh planner session".into(),
            ));
            let sid = new_session_id()?;
            st.planner_session_id = sid.clone();
            let text = planner_lane(
                &cfg,
                &repo,
                &run_dir,
                lane::Session::Create(sid),
                lane::replanner_prompt(&st.title, &prev, feedback, &cfg.commands.test),
                tx,
            )?;
            parse_planner_spec(&text, &cfg.commands.test)?
        }
        Err(e) => return Err(e),
    };
    std::fs::write(run_dir.join("spec.json"), serde_json::to_string_pretty(&sp)?)?;
    st.gates.spec = Default::default();
    // The tests and the implementation were derived from the spec that just
    // died, so the approvals on them were for a feature that no longer exists.
    // Without this a replan + re-run inherits yesterday's ✓ and commit will ship
    // a diff no human ever read — the one thing the gates exist to prevent.
    // The artifacts themselves stay: evidence is never destroyed, and the run
    // screen labels them as superseded until a re-run replaces them.
    let downstream = st.gates.tests.approved || st.gates.work.approved;
    st.gates.tests = Default::default();
    st.gates.work = Default::default();
    st.status = Status::Planned;
    st.save(&run_dir)?;
    if downstream {
        log.append(
            "gate_reset",
            json!({"gate": "tests,work", "why": "spec revised — the diffs they approved came from the old one"}),
        )?;
    }
    log.append("plan_revised", json!({"spec_files": sp.files}))?;
    let _ = tx.send(Progress::RunCreated { id: st.id.clone() });
    let _ = tx.send(Progress::Done("spec revised — read it and approve it".into()));
    Ok(0)
}

pub fn run(id: &str, tx: &Sender<Progress>) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    if let Some(w) = decorrelation_warning(&cfg) {
        let _ = tx.send(Progress::Stage(w));
    }
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    let sp = Spec::load(&run_dir.join("spec.json"))?;
    if !st.gates.spec.approved {
        bail!(
            "spec not approved. Read {} then `guvnor approve {} --gate spec`",
            run_dir.join("spec.json").display(),
            st.id
        );
    }
    let spec_bytes = std::fs::read(run_dir.join("spec.json"))?;
    if digest::sha256_hex(&spec_bytes) != st.gates.spec.sha256 {
        bail!(
            "spec.json changed since approval — re-approve: guvnor approve {} --gate spec",
            st.id
        );
    }
    let log = events::EventLog::new(&run_dir);
    let test_cmd = &sp.verification;
    let timeout = Duration::from_secs(cfg.limits.lane_timeout_secs);

    // Keep the worktree container (.guvnor/wt/) out of git before we touch it.
    worktree::ensure_wt_ignored(&repo)?;

    // A fresh `git init` has no HEAD; worktrees and the evidence digests all
    // need a baseline tree. Bootstrap one from the current tree so a brand-new
    // repo can run (the human still owns every later commit).
    if digest::ensure_baseline_commit(&repo)? {
        let _ = tx.send(Progress::Stage(
            "no commits yet — created a baseline commit so lanes have a base tree".into(),
        ));
        log.append("baseline_commit", json!({"created": true}))?;
    }

    // Worktrees: verification tree + one per writer lane. Cleaned on success,
    // kept for inspection on failure.
    let wt_verif = worktree::create(&repo, &st.id, "verif")?;

    // Gate 0: baseline must be green, else red proves nothing.
    let _ = tx.send(Progress::Stage(format!("[0/5] baseline check: {test_cmd}")));
    let base = harness::run_tests(&wt_verif, test_cmd)?;
    log.append("baseline", json!({"green": base.green, "exit": base.exit_code}))?;
    let _ = tx.send(Progress::GateResult {
        gate: "baseline".into(),
        ok: base.green,
        detail: if base.green { String::new() } else { base.tail.clone() },
    });
    if !base.green {
        return fail(&run_dir, &mut st, &log, tx, "vacuous_baseline", base.tail);
    }

    // Lane 1: test-writer (cold, spec-only, tests paths only).
    let _ = tx.send(Progress::Stage(format!(
        "[1/5] test-writer lane ({}) ...",
        cfg.claude.model_worker
    )));
    let wt_tests = worktree::create(&repo, &st.id, "tests")?;
    lane::write_settings(&wt_tests, &[])?;
    let before = digest::capture(&wt_tests)?;
    let tres = lane::run(lane::LaneSpec {
        cwd: &wt_tests,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_worker,
        prompt: lane::testwriter_prompt(&sp.render(), &cfg.paths.tests, test_cmd),
        allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
        timeout,
        transcript: run_dir.join("lanes-tests.ndjson"),
        line_sink: lane_sink(tx, "test-writer"),
        session: lane::Session::Ephemeral,
    })?;
    lane_report("test-writer", &tres, tx);
    let after = digest::capture(&wt_tests)?;
    let changed = digest::verdict(&before, &after)?;
    let mut ev = json!({"changed": changed, "denials": tres.denials, "timed_out": tres.timed_out, "exit": tres.exit_code, "secs": tres.duration_secs});
    merge_json(&mut ev, usage_json(&tres));
    log.append("lane_tests", ev)?;
    if tres.cancelled {
        return fail(&run_dir, &mut st, &log, tx, "cancelled", "cancelled during test-writer lane".into());
    }
    if tres.timed_out {
        return fail(&run_dir, &mut st, &log, tx, "tests_lane_timeout", String::new());
    }
    if !changed {
        return fail(&run_dir, &mut st, &log, tx, "tests_lane_noop", noop_detail(&tres));
    }
    let tests_patch = worktree::capture_patch(&wt_tests)?;
    if let Err(e) = worktree::validate_patch(&tests_patch, "tests") {
        return fail(&run_dir, &mut st, &log, tx, "tests_forbidden_paths", format!("{e:#}"));
    }
    std::fs::write(run_dir.join("tests.patch"), &tests_patch)?;
    st.tests_patch_sha256 = digest::sha256_hex(tests_patch.as_bytes());
    // From here the artifacts on disk belong to this spec, and a later replan
    // is detectable by comparing this against spec.json.
    st.spec_sha_at_run = st.gates.spec.sha256.clone();

    // Gate 2 (red): tests must FAIL on base.
    let _ = tx.send(Progress::Stage("[2/5] red gate: tests must fail on base".into()));
    worktree::apply_patch(&wt_verif, &tests_patch)?;
    let red = harness::run_tests(&wt_verif, test_cmd)?;
    log.append("red_gate", json!({"green": red.green, "exit": red.exit_code}))?;
    let _ = tx.send(Progress::GateResult {
        gate: "red".into(),
        ok: !red.green,
        detail: if red.green { "tests pass without any implementation".into() } else { String::new() },
    });
    if red.green {
        return fail(&run_dir, &mut st, &log, tx, "vacuous_tests", "tests pass without any implementation".into());
    }
    st.red_reason = red.tail.clone();
    st.status = Status::RedOk;
    st.save(&run_dir)?;

    // Lane 2: implementer (cold, spec-only, src paths only, never sees tests).
    let wt_impl = worktree::create(&repo, &st.id, "impl")?;
    // The implementer may write the whole repo EXCEPT the paths tests.patch
    // already owns. Without this the spec's Files list (which names test files)
    // walks it straight into a non-composable patch: both patches create the
    // same file, and `git apply` on the verif tree dies with "already exists".
    lane::write_settings(&wt_impl, &worktree::patch_paths(&tests_patch))?;
    let impl_patch = if seed_impl(&wt_impl, &run_dir, &tests_patch) {
        // Amend path: the previous run's implementation is in the tree. The
        // green gate and the rework loop below adapt it to the new tests, which
        // is most of what a cold pass would have done anyway — and the reviewer
        // still judges the result against the spec, so anything the amended
        // spec dropped is still caught.
        let _ = tx.send(Progress::Stage(
            "[3/5] reusing the previous implementation — no cold implementer pass".into(),
        ));
        log.append("impl_seeded", json!({"from": "impl.patch"}))?;
        worktree::capture_patch(&wt_impl)?
    } else {
        let _ = tx.send(Progress::Stage(format!(
            "[3/5] implementer lane ({}) ...",
            cfg.claude.model_worker
        )));
        let before = digest::capture(&wt_impl)?;
        let ires = lane::run(lane::LaneSpec {
            cwd: &wt_impl,
            claude_bin: &cfg.claude.bin,
            model: &cfg.claude.model_worker,
            prompt: lane::implementer_prompt(&sp.render(), &cfg.paths.src, test_cmd),
            allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
            timeout,
            transcript: run_dir.join("lanes-impl.ndjson"),
            line_sink: lane_sink(tx, "implementer"),
            session: lane::Session::Ephemeral,
        })?;
        lane_report("implementer", &ires, tx);
        let after = digest::capture(&wt_impl)?;
        let changed = digest::verdict(&before, &after)?;
        let mut ev = json!({"changed": changed, "denials": ires.denials, "timed_out": ires.timed_out, "exit": ires.exit_code, "secs": ires.duration_secs});
        merge_json(&mut ev, usage_json(&ires));
        log.append("lane_impl", ev)?;
        if ires.cancelled {
            return fail(&run_dir, &mut st, &log, tx, "cancelled", "cancelled during implementer lane".into());
        }
        if ires.timed_out {
            return fail(&run_dir, &mut st, &log, tx, "impl_lane_timeout", String::new());
        }
        if !changed {
            return fail(&run_dir, &mut st, &log, tx, "impl_lane_noop", noop_detail(&ires));
        }
        worktree::capture_patch(&wt_impl)?
    };
    if let Err(e) = worktree::validate_patch(&impl_patch, "impl") {
        return fail(&run_dir, &mut st, &log, tx, "impl_forbidden_paths", format!("{e:#}"));
    }
    // Backstop for the hook: never let git apply report this as a raw error.
    let overlap = worktree::overlapping_paths(&tests_patch, &impl_patch);
    if !overlap.is_empty() {
        return fail(&run_dir, &mut st, &log, tx, "impl_touched_test_files", overlap_detail(&overlap));
    }
    std::fs::write(run_dir.join("impl.patch"), &impl_patch)?;
    st.impl_patch_sha256 = digest::sha256_hex(impl_patch.as_bytes());

    // Gate 4 (green): tests must PASS with the implementation. On failure the
    // implementer lane gets the failing output back — evidence-driven, bounded
    // by limits.max_rework_rounds, never blind.
    let mut impl_patch = impl_patch;
    let max_rework = cfg.limits.max_rework_rounds;
    let mut round: u64 = 0;
    loop {
        let _ = tx.send(Progress::Stage(if round == 0 {
            "[4/5] green gate: tests must pass with implementation".into()
        } else {
            format!("[4/5] green gate: re-check after rework {round}/{max_rework}")
        }));
        worktree::apply_patch(&wt_verif, &impl_patch)?;
        let green = harness::run_tests(&wt_verif, test_cmd)?;
        log.append("green_gate", json!({"green": green.green, "exit": green.exit_code, "round": round}))?;
        std::fs::write(run_dir.join("green.txt"), &green.tail)?;
        let _ = tx.send(Progress::GateResult {
            gate: "green".into(),
            ok: green.green,
            detail: if green.green { String::new() } else { green.tail.clone() },
        });
        if green.green {
            break;
        }
        if round >= max_rework {
            return fail(
                &run_dir,
                &mut st,
                &log,
                tx,
                "impl_does_not_satisfy_tests",
                format!("{}\n\nrework budget spent ({max_rework})", green.tail),
            );
        }
        round += 1;
        let _ = tx.send(Progress::Stage(format!(
            "[3/5] rework {round}/{max_rework}: implementer gets the failing output"
        )));
        log.append("rework_started", json!({"round": round}))?;
        let before = digest::capture(&wt_impl)?;
        let rres = lane::run(lane::LaneSpec {
            cwd: &wt_impl,
            claude_bin: &cfg.claude.bin,
            model: &cfg.claude.model_worker,
            prompt: lane::rework_prompt(
                &sp.render(),
                &cfg.paths.src,
                test_cmd,
                &green.tail,
                round,
                max_rework,
            ),
            allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
            timeout,
            transcript: run_dir.join(format!("lanes-impl-rework{round}.ndjson")),
            line_sink: lane_sink(tx, "implementer rework"),
            session: lane::Session::Ephemeral,
        })?;
        lane_report("implementer rework", &rres, tx);
        let after = digest::capture(&wt_impl)?;
        let changed = digest::verdict(&before, &after)?;
        let mut ev = json!({"round": round, "changed": changed, "denials": rres.denials, "timed_out": rres.timed_out, "exit": rres.exit_code, "secs": rres.duration_secs});
        merge_json(&mut ev, usage_json(&rres));
        log.append("lane_impl_rework", ev)?;
        if rres.cancelled {
            return fail(&run_dir, &mut st, &log, tx, "cancelled", "cancelled during rework lane".into());
        }
        if rres.timed_out {
            return fail(&run_dir, &mut st, &log, tx, "rework_lane_timeout", String::new());
        }
        if !changed {
            return fail(
                &run_dir,
                &mut st,
                &log,
                tx,
                "impl_does_not_satisfy_tests",
                format!("rework round {round} made no edits\n\n{}", green.tail),
            );
        }
        let p = worktree::capture_patch(&wt_impl)?;
        if let Err(e) = worktree::validate_patch(&p, "impl") {
            return fail(&run_dir, &mut st, &log, tx, "impl_forbidden_paths", format!("{e:#}"));
        }
        let overlap = worktree::overlapping_paths(&tests_patch, &p);
        if !overlap.is_empty() {
            return fail(&run_dir, &mut st, &log, tx, "impl_touched_test_files", overlap_detail(&overlap));
        }
        std::fs::write(run_dir.join("impl.patch"), &p)?;
        st.impl_patch_sha256 = digest::sha256_hex(p.as_bytes());
        impl_patch = p;
        // verif tree back to base + tests before re-applying the cumulative patch
        worktree::reset_hard(&wt_verif)?;
        worktree::apply_patch(&wt_verif, &tests_patch)?;
    }
    st.status = Status::GreenOk;
    st.save(&run_dir)?;

    // Lane 3: cold reviewer over the combined diff, in the final tree.
    review_and_finish(&cfg, &repo, &run_dir, &mut st, &sp, &wt_verif, &tests_patch, &impl_patch, tx)
}

/// Cold reviewer over the combined diff, then terminal bookkeeping. Shared by
/// the initial run and a findings-driven fix round: each ends with a verdict
/// bound to sha256 of the diff it actually judged, so commit can never ship a
/// diff that no reviewer saw.
#[allow(clippy::too_many_arguments)]
fn review_and_finish(
    cfg: &Config,
    repo: &Path,
    run_dir: &Path,
    st: &mut State,
    sp: &Spec,
    wt_verif: &Path,
    tests_patch: &str,
    impl_patch: &str,
    tx: &Sender<Progress>,
) -> Result<i32> {
    let log = events::EventLog::new(run_dir);
    let _ = tx.send(Progress::Stage(format!(
        "[5/5] reviewer lane ({}) ...",
        cfg.claude.model_reviewer
    )));
    let combined = format!("{tests_patch}\n{impl_patch}");
    // The reviewer has no shell (a claim to have run tests proves nothing — the
    // green gate already ran them). Hand it the gate's own output, written to
    // green.txt by both callers immediately above, or it cannot judge a
    // "tests pass" criterion and will file its denied Bash as a finding.
    let green = std::fs::read_to_string(run_dir.join("green.txt")).unwrap_or_default();
    let rres = lane::run(lane::LaneSpec {
        cwd: wt_verif,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_reviewer,
        prompt: lane::reviewer_prompt(&sp.render(), &combined, &sp.verification, &green),
        allowed_tools: "Read,Glob,Grep",
        timeout: Duration::from_secs(cfg.limits.lane_timeout_secs),
        transcript: run_dir.join("lanes-review.ndjson"),
        line_sink: lane_sink(tx, "reviewer"),
        session: lane::Session::Ephemeral,
    })?;
    lane_report("reviewer", &rres, tx);
    if rres.cancelled {
        return fail(run_dir, st, &log, tx, "cancelled", "cancelled during reviewer lane".into());
    }
    if rres.timed_out {
        return fail(run_dir, st, &log, tx, "review_timeout", String::new());
    }
    let verdict = match review::parse_verdict(&rres.result_text) {
        Ok(v) => v,
        Err(e) => return fail(run_dir, st, &log, tx, "review_unparseable", format!("{e:#}")),
    };
    let review = review::Review {
        verdict,
        diff_sha256: digest::sha256_hex(combined.as_bytes()),
        model: cfg.claude.model_reviewer.clone(),
        ts: events::now_iso(),
    };
    std::fs::write(run_dir.join("review.json"), serde_json::to_string_pretty(&review)?)?;
    let mut ev = json!({"verdict": review.verdict.verdict.to_string()});
    merge_json(&mut ev, usage_json(&rres));
    log.append("reviewed", ev)?;
    st.status = Status::Reviewed;
    st.save(run_dir)?;

    // Success: throwaway trees go away; patches + evidence remain.
    worktree::remove_run(repo, &st.id)?;
    let _ = tx.send(Progress::Done(format!(
        "\nverdict: {v} — case file:\n  guvnor review {id}\nthen:  guvnor approve {id} --gate tests   (do these test the spec, not trivia?)\n       guvnor approve {id} --gate work    (is the implementation right?)\n       guvnor commit {id} -m \"...\"",
        v = review.verdict.verdict,
        id = st.id
    )));
    Ok(0)
}

/// True when every ticked finding names a test-file path and there is nothing
/// else to act on. The fix lane runs on base + impl.patch and never sees (or may
/// touch) the tests, so a round aimed only at test files can only no-op.
fn all_findings_are_tests(findings: &[review::Finding], tests: &[String]) -> bool {
    !findings.is_empty()
        && findings
            .iter()
            .all(|f| tests.iter().any(|t| f.file.starts_with(t.as_str())))
}

/// Fix round: the human picked which reviewer findings matter; the implementer
/// addresses exactly those, the green gate re-checks the tests still pass, and
/// the reviewer runs again on the new diff.
///
/// Any prior `work` approval dies here by construction — the diff it approved
/// no longer exists. The `tests` approval survives: tests.patch is untouched.
pub fn fix(
    id: &str,
    findings: &[review::Finding],
    note: &str,
    tx: &Sender<Progress>,
) -> Result<i32> {
    if findings.is_empty() && note.trim().is_empty() {
        bail!("nothing to fix: no findings selected and no instruction given");
    }
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    // Landing stages the patches against the tree they were cut from. Once that
    // has happened the patches no longer apply to a fresh worktree, so a fix
    // round would die inside `git apply` — refuse with a reason instead.
    if matches!(st.status, Status::Staged | Status::Committed) {
        bail!("run already landed in the repo — fix it as a new feature, not a fix round");
    }
    // A fix round aimed only at test files is a dead end: the lane runs without
    // the tests and may never touch them, so it edits nothing and reports a
    // no-op. Refuse up front and point at the two moves that can act.
    if note.trim().is_empty() && all_findings_are_tests(findings, &cfg.paths.tests) {
        bail!(
            "every ticked finding is about a test file, and the fix lane can't touch tests. \
             Untick them, or use `change the spec` to adjust what the tests must assert."
        );
    }
    let sp = Spec::load(&run_dir.join("spec.json"))?;
    let log = events::EventLog::new(&run_dir);
    let test_cmd = &sp.verification;

    let tests_patch = std::fs::read_to_string(run_dir.join("tests.patch"))
        .context("tests.patch missing — nothing to fix against")?;
    let impl_patch = std::fs::read_to_string(run_dir.join("impl.patch"))
        .context("impl.patch missing — nothing to fix")?;
    // The keys, not just the count: when a fix breaks the suite the Failure tab
    // has to be able to name which finding it was told to fix, because the
    // conflict between that finding and a test IS the failure.
    log.append(
        "fix_started",
        json!({
            "findings": findings.len(),
            "note": note,
            "ticked": findings
                .iter()
                .map(|f| json!({"file": f.file, "note": f.note}))
                .collect::<Vec<_>>(),
        }),
    )?;

    // The run's throwaway trees were removed on success — rebuild the two we
    // need: the implementer's tree (base + its own patch) and the verification
    // tree (base + tests), ready for the green re-check.
    worktree::ensure_wt_ignored(&repo)?;
    let wt_impl = worktree::create(&repo, &st.id, "impl")?;
    worktree::apply_patch(&wt_impl, &impl_patch)?;
    let wt_verif = worktree::create(&repo, &st.id, "verif")?;
    worktree::apply_patch(&wt_verif, &tests_patch)?;
    // Same fence as the original implementer lane: everything except the paths
    // tests.patch owns, so the two patches stay composable.
    lane::write_settings(&wt_impl, &worktree::patch_paths(&tests_patch))?;

    // The fix lane cannot see the tests (that is the decorrelation), so a
    // finding that contradicts one takes the suite down and the lane never
    // learns why. Same answer the run's green gate already has: hand the failing
    // output back and let it try again, bounded by the same budget. Without this
    // the human had to read the failure and re-type it into the instruction box
    // — evidence the engine already had on disk.
    let max_rework = cfg.limits.max_rework_rounds;
    let mut broke: Option<String> = None;
    let mut round: u64 = 0;
    let new_impl = loop {
        let _ = tx.send(Progress::Stage(match round {
            0 => format!(
                "[1/2] fix lane ({}): {} finding(s){} ...",
                cfg.claude.model_worker,
                findings.len(),
                if note.trim().is_empty() { "" } else { " + your instruction" }
            ),
            r => format!("[1/2] fix rework {r}/{max_rework}: the lane gets the failing test back"),
        }));
        if round > 0 {
            log.append("fix_rework_started", json!({"round": round}))?;
        }
        let before = digest::capture(&wt_impl)?;
        let fres = lane::run(lane::LaneSpec {
            cwd: &wt_impl,
            claude_bin: &cfg.claude.bin,
            model: &cfg.claude.model_worker,
            prompt: lane::fix_prompt(
                &sp.render(),
                &cfg.paths.src,
                test_cmd,
                findings,
                &st.fixed_findings,
                note,
                broke.as_deref(),
            ),
            allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
            timeout: Duration::from_secs(cfg.limits.lane_timeout_secs),
            transcript: run_dir.join(if round == 0 {
                "lanes-impl-fix.ndjson".to_string()
            } else {
                format!("lanes-impl-fix-rework{round}.ndjson")
            }),
            line_sink: lane_sink(tx, "fix"),
            session: lane::Session::Ephemeral,
        })?;
        lane_report("fix", &fres, tx);
        let changed = digest::verdict(&before, &digest::capture(&wt_impl)?)?;
        let mut ev = json!({"changed": changed, "denials": fres.denials, "timed_out": fres.timed_out, "exit": fres.exit_code, "secs": fres.duration_secs, "round": round});
        merge_json(&mut ev, usage_json(&fres));
        log.append("lane_impl_fix", ev)?;
        if fres.cancelled {
            return fail(&run_dir, &mut st, &log, tx, "cancelled", "cancelled during fix lane".into());
        }
        if fres.timed_out {
            return fail(&run_dir, &mut st, &log, tx, "fix_lane_timeout", String::new());
        }
        if !changed {
            // A rework round that edits nothing is the lane standing by its last
            // answer: report the regression, not the no-op, or the advice sends
            // the human after the wrong thing.
            return match &broke {
                None => fail(&run_dir, &mut st, &log, tx, "fix_lane_noop", noop_detail(&fres)),
                Some(tail) => fail(
                    &run_dir,
                    &mut st,
                    &log,
                    tx,
                    "fix_broke_tests",
                    format!("{tail}\n\n{}\n\n{BROKE_TAIL}", noop_detail(&fres)),
                ),
            };
        }
        let new_impl = worktree::capture_patch(&wt_impl)?;
        if let Err(e) = worktree::validate_patch(&new_impl, "impl") {
            return fail(&run_dir, &mut st, &log, tx, "impl_forbidden_paths", format!("{e:#}"));
        }
        let overlap = worktree::overlapping_paths(&tests_patch, &new_impl);
        if !overlap.is_empty() {
            return fail(&run_dir, &mut st, &log, tx, "impl_touched_test_files", overlap_detail(&overlap));
        }

        // Green gate again: a fix that breaks the tests is not a fix.
        let _ = tx.send(Progress::Stage("[2/2] green gate: tests must still pass".into()));
        // Every attempt is judged against the same base: base + tests only, or
        // the previous attempt's edits would still be in the tree.
        worktree::reset_hard(&wt_verif)?;
        worktree::apply_patch(&wt_verif, &tests_patch)?;
        worktree::apply_patch(&wt_verif, &new_impl)?;
        let green = harness::run_tests(&wt_verif, test_cmd)?;
        log.append("green_gate", json!({"green": green.green, "exit": green.exit_code, "after": "fix", "round": round}))?;
        std::fs::write(run_dir.join("green.txt"), &green.tail)?;
        let _ = tx.send(Progress::GateResult {
            gate: "green".into(),
            ok: green.green,
            detail: if green.green { String::new() } else { green.tail.clone() },
        });
        if green.green {
            break new_impl;
        }
        if round >= max_rework {
            return fail(
                &run_dir,
                &mut st,
                &log,
                tx,
                "fix_broke_tests",
                format!("{}\n\n{BROKE_TAIL} (rework budget spent: {max_rework})", green.tail),
            );
        }
        // Throw the attempt away: the tree goes back to the implementation that
        // passed, so the next round starts from working code, not broken code.
        worktree::reset_hard(&wt_impl)?;
        worktree::apply_patch(&wt_impl, &impl_patch)?;
        broke = Some(green.tail);
        round += 1;
    };
    std::fs::write(run_dir.join("impl.patch"), &new_impl)?;
    st.impl_patch_sha256 = digest::sha256_hex(new_impl.as_bytes());
    // The approved work no longer exists — that verdict was for the old diff.
    st.gates.work = Default::default();
    st.status = Status::GreenOk;
    // Dealt with: don't ask about these again. If the fresh review re-raises
    // one, the UI shows it as re-raised rather than pretending it's resolved.
    let known: Vec<String> = st.fixed_findings.iter().map(state::finding_key).collect();
    for f in findings {
        if !known.contains(&state::finding_key(f)) {
            st.fixed_findings.push(f.clone());
        }
    }
    st.save(&run_dir)?;
    log.append("gate_reset", json!({"gate": "work", "why": "impl changed by fix round"}))?;

    review_and_finish(&cfg, &repo, &run_dir, &mut st, &sp, &wt_verif, &tests_patch, &new_impl, tx)
}

pub fn set_gate(id: &str, gate: state::Gate, note: &str, approve: bool) -> Result<String> {
    let repo = config::find_repo_root()?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    let slot = st.gates.slot_mut(gate);
    slot.approved = approve;
    slot.ts = events::now_iso();
    slot.note = note.to_string();
    if approve && gate == state::Gate::Spec {
        // Re-validate after human edits and bind the approval to this exact content.
        Spec::load(&run_dir.join("spec.json"))?;
        let bytes = std::fs::read(run_dir.join("spec.json"))?;
        st.gates.spec.sha256 = digest::sha256_hex(&bytes);
        st.status = Status::SpecApproved;
        // Spec accepted — close the iterating planner session.
        st.planner_session_id.clear();
    }
    if !approve {
        st.status = Status::Failed(format!("rejected_{}", gate.as_str()));
    }
    st.save(&run_dir)?;
    events::EventLog::new(&run_dir).append(
        if approve { "gate_approved" } else { "gate_rejected" },
        json!({"gate": gate.as_str(), "note": note}),
    )?;
    Ok(format!(
        "{} {} gate for {}",
        if approve { "approved" } else { "rejected" },
        gate.as_str(),
        st.id
    ))
}

/// Everything that must be true before a run's patches may touch your repo.
/// Returns the two patches, in the order they apply. Reads only.
fn commit_checks(repo: &Path, run_dir: &Path, st: &State) -> Result<(String, String)> {
    let (tests_patch, impl_patch) = approval_checks(run_dir, st)?;
    let status = digest::git(repo, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        bail!("main repo tree is dirty; commit or stash first");
    }
    Ok((tests_patch, impl_patch))
}

/// The half of the checks that is about the *approval*, not the tree: three
/// gates, and patches that still hash to what the reviewer read. Separate from
/// the tree condition because staging needs a clean tree and the commit after it
/// needs the opposite.
///
/// The verdict is not one of the checks: the reviewer holds no gate, the human
/// judges the diff at the work gate having read it.
fn approval_checks(run_dir: &Path, st: &State) -> Result<(String, String)> {
    for (name, a) in [("spec", &st.gates.spec), ("tests", &st.gates.tests), ("work", &st.gates.work)] {
        if !a.approved {
            bail!("gate '{name}' not approved — approve all three first");
        }
    }
    let review: review::Review =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("review.json"))?)
            .context("review.json missing/invalid — run not reviewed")?;
    let tests_patch = std::fs::read_to_string(run_dir.join("tests.patch"))?;
    let impl_patch = std::fs::read_to_string(run_dir.join("impl.patch"))?;
    // The reviewer's verdict is bound to a digest of the exact bytes it read.
    // Without this an approval could slide onto a different diff.
    let combined = format!("{tests_patch}\n{impl_patch}");
    if digest::sha256_hex(combined.as_bytes()) != review.diff_sha256 {
        bail!("patches on disk do not match the reviewed diff digest — stale or tampered; re-run");
    }
    Ok((tests_patch, impl_patch))
}

/// Hash of the index: `git write-tree` is git's own canonical answer to "what
/// exactly is staged", and it ignores untracked files, so an unrelated scratch
/// file in your tree can't invalidate a staging.
fn index_tree(repo: &Path) -> Result<String> {
    Ok(digest::git(repo, &["write-tree"])?.trim().to_string())
}

/// Is the index still exactly what `stage` put there? Everything guvnor does
/// after staging — the commit, the unstage — is only guvnor's to do while this
/// holds. Past it, the staged change is yours.
fn staging_intact(repo: &Path, st: &State) -> Result<bool> {
    if st.status != Status::Staged || st.staged_tree.is_empty() {
        return Ok(false);
    }
    Ok(index_tree(repo)? == st.staged_tree)
}

const DRIFTED: &str = "the staged change is no longer the one that was reviewed — \
commit it yourself with git, or `git reset` and stage again";

/// Every `fix_broke_tests` says this: the attempt is gone and the implementation
/// that passed is still the one on disk. Stated once so the failure and the
/// advice cannot drift apart about what state the run is in.
const BROKE_TAIL: &str =
    "the fix regressed the suite; it was thrown away and impl.patch on disk is unchanged";

/// The files a commit would touch, for the human to read before it happens.
/// Names only — the diffs were already judged on the Tests and Work tabs.
pub fn commit_files(id: &str) -> Result<Vec<String>> {
    commit_files_at(&config::find_repo_root()?, id)
}

fn commit_files_at(repo: &Path, id: &str) -> Result<Vec<String>> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut files = Vec::new();
    for f in ["tests.patch", "impl.patch"] {
        for p in worktree::patch_paths(&std::fs::read_to_string(run_dir.join(f))?) {
            if !files.contains(&p) {
                files.push(p);
            }
        }
    }
    Ok(files)
}

/// Draft a commit message from the spec and the reviewed diff. Sent back as
/// `Progress::Done` so the caller can put it in front of the human to edit —
/// guvnor proposes the words, it never decides them.
pub fn commit_message(id: &str, tx: &Sender<Progress>) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let sp = Spec::load(&run_dir.join("spec.json"))?;
    let diff = format!(
        "{}\n{}",
        std::fs::read_to_string(run_dir.join("tests.patch"))?,
        std::fs::read_to_string(run_dir.join("impl.patch"))?
    );
    let _ = tx.send(Progress::Stage(format!(
        "drafting a commit message ({}) ...",
        cfg.claude.model_worker
    )));
    let res = lane::run(lane::LaneSpec {
        cwd: &repo,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_worker,
        // The objective only — one line of why. The rest of the spec (criteria,
        // interfaces, verification) is guvnor's process, and none of it is
        // committed: in `git log` a year from now it names nothing that exists.
        prompt: lane::commit_msg_prompt(sp.objective.trim(), &diff),
        // Reads only: it has the diff in the prompt and nothing to write.
        allowed_tools: "Read,Glob,Grep",
        timeout: Duration::from_secs(cfg.limits.lane_timeout_secs),
        transcript: run_dir.join("lanes-commitmsg.ndjson"),
        line_sink: lane_sink(tx, "commit message"),
        session: lane::Session::Ephemeral,
    })?;
    lane_report("commit message", &res, tx);
    events::EventLog::new(&run_dir).append("commit_message", usage_json(&res))?;
    if res.cancelled {
        bail!("cancelled");
    }
    if res.timed_out {
        bail!("timed out drafting the commit message");
    }
    let msg = res.result_text.trim();
    if msg.is_empty() {
        bail!("the lane returned no message");
    }
    let _ = tx.send(Progress::Done(msg.to_string()));
    Ok(0)
}

/// Split a git-shaped message into subject and body: the first line, then
/// whatever follows it. Same shape `commit_msg_prompt` asks for, so a generated
/// message and a hand-typed one split identically.
pub fn split_commit_message(msg: &str) -> (&str, &str) {
    let msg = msg.trim();
    match msg.split_once('\n') {
        Some((subject, rest)) => (subject.trim_end(), rest.trim()),
        None => (msg, ""),
    }
}

/// Apply a run's patches to your working tree and index, and stop there. This
/// is where guvnor's job ends: the change is in your project, so you can open
/// the files, run the thing, and decide. `commit` and `unstage` are what happens
/// next, and both are yours to pick.
///
/// Idempotent enough to be safe: staging a run that is already staged, after you
/// reset the index yourself, just applies it again.
pub fn stage(id: &str) -> Result<String> {
    stage_at(&config::find_repo_root()?, id)
}

fn stage_at(repo: &Path, id: &str) -> Result<String> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if staging_intact(repo, &st)? {
        return Ok("already staged — `git diff --cached` to read it".into());
    }
    // Staged, but the index no longer hashes to what we put there. If the tree
    // came back clean you reset it away, and staging afresh is exactly right;
    // anything still in there is yours now, and not ours to overwrite.
    if st.status == Status::Staged && !digest::git(repo, &["status", "--porcelain"])?.trim().is_empty()
    {
        bail!("{DRIFTED}");
    }
    let (tests_patch, impl_patch) = commit_checks(repo, &run_dir, &st)?;
    worktree::apply_patch_staged(repo, &tests_patch)?;
    worktree::apply_patch_staged(repo, &impl_patch)?;
    st.staged_tree = index_tree(repo)?;
    st.status = Status::Staged;
    st.save(&run_dir)?;
    let n = commit_files_at(repo, id)?.len();
    events::EventLog::new(&run_dir).append("staged", json!({"files": n, "tree": st.staged_tree}))?;
    Ok(format!(
        "staged {n} file(s) in your working tree — read them, run them, then commit (or unstage)"
    ))
}

/// Take it back out: reverse-apply both patches, leaving the tree as it was
/// before staging. Only while the staging is untouched — once you have edited
/// it, undoing it is a decision about your own work and git is the right tool.
///
/// The patches and every other artifact stay on disk, so the run can be staged
/// again, or sent back through a fix round.
pub fn unstage(id: &str) -> Result<String> {
    unstage_at(&config::find_repo_root()?, id)
}

fn unstage_at(repo: &Path, id: &str) -> Result<String> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if st.status != Status::Staged {
        bail!("nothing staged for this run");
    }
    if !staging_intact(repo, &st)? {
        bail!("{DRIFTED}");
    }
    let tests_patch = std::fs::read_to_string(run_dir.join("tests.patch"))?;
    let impl_patch = std::fs::read_to_string(run_dir.join("impl.patch"))?;
    // Reverse order, so the tree passes back through the states it came by.
    worktree::reverse_patch_staged(repo, &impl_patch)?;
    worktree::reverse_patch_staged(repo, &tests_patch)?;
    st.staged_tree = String::new();
    st.status = Status::Reviewed;
    st.save(&run_dir)?;
    events::EventLog::new(&run_dir).append("unstaged", json!({}))?;
    Ok("unstaged — your tree is back as it was; the patches are still on disk".into())
}

/// Write the commit. Only ever the staged change, and only while the index still
/// hashes to what `stage` put there — a commit guvnor signs is a commit a
/// reviewer read. A run that hasn't been staged yet is staged first, so the
/// one-liner `guvnor commit <id> -m "..."` still works end to end.
///
/// An empty subject means "stage only", which is what this verb used to do
/// without one. Guv'nor never pushes; that boundary is not configurable.
pub fn commit(id: &str, subject: &str, body: &str) -> Result<String> {
    commit_at(&config::find_repo_root()?, id, subject, body)
}

fn commit_at(repo: &Path, id: &str, subject: &str, body: &str) -> Result<String> {
    let subject = subject.trim();
    if subject.is_empty() {
        return stage_at(repo, id);
    }
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if st.status != Status::Staged {
        stage_at(repo, id)?;
        st = State::load(&run_dir)?;
    }
    if !staging_intact(repo, &st)? {
        bail!("{DRIFTED}");
    }
    approval_checks(&run_dir, &st)?;
    // Two -m arguments is how git writes "subject\n\nbody" — no temp file, and
    // no shell quoting to get wrong.
    let mut args = vec!["commit", "-m", subject];
    let body = body.trim();
    if !body.is_empty() {
        args.push("-m");
        args.push(body);
    }
    digest::git(repo, &args)
        .context("git commit failed (is user.name/user.email set? did a pre-commit hook refuse?)")?;
    let sha = digest::git(repo, &["rev-parse", "--short", "HEAD"])?.trim().to_string();
    st.staged_tree = String::new();
    st.status = Status::Committed;
    st.save(&run_dir)?;
    events::EventLog::new(&run_dir)
        .append("committed", json!({"sha": sha, "subject": subject, "body": !body.is_empty()}))?;
    Ok(format!("committed {sha} — guvnor does not push, that one is yours"))
}

/// Files staged for this run that you have since edited without staging the
/// edit. `git commit` takes the index, so those edits are simply not in the
/// commit — worth saying out loud before it happens, but not worth refusing.
pub fn unstaged_edits(id: &str) -> Result<Vec<String>> {
    unstaged_edits_at(&config::find_repo_root()?, id)
}

fn unstaged_edits_at(repo: &Path, id: &str) -> Result<Vec<String>> {
    let owned = commit_files_at(repo, id)?;
    let mut out = Vec::new();
    for line in digest::git(repo, &["status", "--porcelain"])?.lines() {
        // XY path: Y is the worktree column, ' ' there means "index and tree agree"
        let (Some(worktree_col), Some(path)) = (line.chars().nth(1), line.get(3..)) else {
            continue;
        };
        if worktree_col != ' ' && owned.iter().any(|f| f == path.trim()) {
            out.push(path.trim().to_string());
        }
    }
    Ok(out)
}

/// Landing: your working tree is the last stop before a commit, and guvnor's
/// signature on that commit is only worth anything while the index still holds
/// the diff a reviewer read.
#[cfg(test)]
mod land_tests {
    use super::*;
    use std::path::PathBuf;

    /// A repo with a baseline commit and one reviewed, fully-approved run whose
    /// patches add a test file and a source file. Patches are generated by git
    /// itself — a hand-rolled one would only prove that the fixture is wrong.
    fn fixture(name: &str) -> (PathBuf, String) {
        fixture_verdict(name, review::Decision::Approved)
    }

    fn fixture_verdict(name: &str, decision: review::Decision) -> (PathBuf, String) {
        let repo = std::env::temp_dir().join(format!("guvnor-land-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&repo).ok();
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| digest::git(&repo, args).unwrap();
        git(&["init", "-q", "."]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        // a signing key in the developer's global config is not this test's
        // business (a real repo keeps its own — guvnor shells out to `git
        // commit` precisely so your settings apply)
        git(&["config", "commit.gpgsign", "false"]);
        // run artifacts live in the repo but are never part of it
        std::fs::create_dir_all(repo.join(".guvnor")).unwrap();
        std::fs::write(repo.join(".guvnor/.gitignore"), "runs/\nwt/\n").unwrap();
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);

        // two patches, cut from real staged files, then wound back
        let patch = |path: &str, body: &str| {
            let full = repo.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, body).unwrap();
            git(&["add", path]);
            let p = digest::git(&repo, &["diff", "--cached"]).unwrap();
            git(&["reset", "-q"]);
            std::fs::remove_file(&full).unwrap();
            p
        };
        let tests_patch = patch("test/a.test.js", "test-ok\n");
        let impl_patch = patch("src/a.js", "impl-ok\n");

        let id = "20260101T000000-land";
        let run_dir = state::runs_root(&repo).join(id);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("tests.patch"), &tests_patch).unwrap();
        std::fs::write(run_dir.join("impl.patch"), &impl_patch).unwrap();
        let combined = format!("{tests_patch}\n{impl_patch}");
        let review = review::Review {
            verdict: review::Verdict {
                verdict: decision,
                summary: "fine".into(),
                findings: vec![],
            },
            diff_sha256: digest::sha256_hex(combined.as_bytes()),
            model: "m".into(),
            ts: "now".into(),
        };
        std::fs::write(run_dir.join("review.json"), serde_json::to_string(&review).unwrap())
            .unwrap();
        let mut st = State::new(id, "landing");
        for g in [&mut st.gates.spec, &mut st.gates.tests, &mut st.gates.work] {
            g.approved = true;
        }
        st.status = Status::Reviewed;
        st.save(&run_dir).unwrap();

        assert_eq!(
            digest::git(&repo, &["status", "--porcelain"]).unwrap().trim(),
            "",
            "fixture must start clean or the staging checks are untested"
        );
        (repo, id.to_string())
    }

    fn status(repo: &Path, id: &str) -> Status {
        State::load(&state::resolve_run_dir(repo, id).unwrap()).unwrap().status
    }

    /// The reviewer holds no gate. Landing used to demand an `APPROVED` verdict,
    /// undoable with `--allow-warning` — two mechanisms cancelling out, and the
    /// flag was CLI-only, so a WARNING run could not be landed from the TUI at
    /// all. The human's judgement is the work gate, made with the verdict on the
    /// screen next to it; the check that carries the weight is the diff digest.
    #[test]
    fn the_verdict_does_not_gate_landing() {
        for d in [review::Decision::Warning, review::Decision::Blocked] {
            let (repo, id) = fixture_verdict(&format!("verdict-{d}"), d);
            assert!(stage_at(&repo, &id).is_ok(), "{d} must be landable");
            assert_eq!(status(&repo, &id), Status::Staged);
            std::fs::remove_dir_all(&repo).ok();
        }
        // ...but a diff that is not the reviewed one still cannot land, whatever
        // the verdict said. That is the check the veto was standing in front of.
        let (repo, id) = fixture_verdict("verdict-tamper", review::Decision::Approved);
        let run_dir = state::resolve_run_dir(&repo, &id).unwrap();
        std::fs::write(run_dir.join("impl.patch"), "tampered\n").unwrap();
        assert!(
            stage_at(&repo, &id).unwrap_err().to_string().contains("do not match"),
            "the digest binding is what protects a landing"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    /// The point of the whole change: staging stops, so you can look.
    #[test]
    fn staging_lands_in_your_tree_and_stops_there() {
        let (repo, id) = fixture("happy");
        let msg = stage_at(&repo, &id).unwrap();
        assert!(msg.contains("2 file(s)"), "{msg}");
        assert_eq!(status(&repo, &id), Status::Staged);
        // the files are really there, and readable with an editor
        assert_eq!(std::fs::read_to_string(repo.join("src/a.js")).unwrap(), "impl-ok\n");
        assert_eq!(std::fs::read_to_string(repo.join("test/a.test.js")).unwrap(), "test-ok\n");
        // ...and no commit was written
        let log = digest::git(&repo, &["log", "--oneline"]).unwrap();
        assert_eq!(log.lines().count(), 1, "staging must not commit: {log}");
        // staging twice is not an error and does not double-apply
        assert!(stage_at(&repo, &id).unwrap().contains("already staged"));

        // then the commit, which is the one guvnor will sign
        let msg = commit_at(&repo, &id, "add a", "why").unwrap();
        assert!(msg.contains("committed"), "{msg}");
        assert_eq!(status(&repo, &id), Status::Committed);
        let log = digest::git(&repo, &["log", "--oneline"]).unwrap();
        assert_eq!(log.lines().count(), 2, "exactly one commit: {log}");
        let body = digest::git(&repo, &["log", "-1", "--pretty=%B"]).unwrap();
        assert!(body.contains("add a") && body.contains("why"), "{body}");
        assert_eq!(digest::git(&repo, &["status", "--porcelain"]).unwrap().trim(), "");
        std::fs::remove_dir_all(&repo).ok();
    }

    /// Guvnor signs what a reviewer read. Once you have edited the staged
    /// change it is your work, and both of guvnor's next moves refuse it.
    #[test]
    fn an_edited_staging_area_refuses_the_commit_and_the_unstage() {
        let (repo, id) = fixture("drift");
        stage_at(&repo, &id).unwrap();
        // your own edit, staged on top
        std::fs::write(repo.join("src/a.js"), "mine now\n").unwrap();
        digest::git(&repo, &["add", "src/a.js"]).unwrap();

        for e in [
            commit_at(&repo, &id, "add a", "").unwrap_err(),
            unstage_at(&repo, &id).unwrap_err(),
            stage_at(&repo, &id).unwrap_err(),
        ] {
            let msg = format!("{e:#}");
            assert!(msg.contains("no longer the one that was reviewed"), "{msg}");
            assert!(msg.contains("git reset"), "must name the way out: {msg}");
        }
        // nothing was committed behind the refusal
        assert_eq!(digest::git(&repo, &["log", "--oneline"]).unwrap().lines().count(), 1);
        assert_eq!(status(&repo, &id), Status::Staged);

        // an unstaged edit is only worth saying out loud: the index is still the
        // reviewed content, so a commit of the index is honest
        std::fs::write(repo.join("test/a.test.js"), "scribbled\n").unwrap();
        assert_eq!(unstaged_edits_at(&repo, &id).unwrap(), ["test/a.test.js"]);
        std::fs::remove_dir_all(&repo).ok();
    }

    /// Looked at it, didn't like it: the way back has to leave no trace in the
    /// tree and no damage to the evidence.
    #[test]
    fn unstage_puts_the_tree_back_and_keeps_the_artifacts() {
        let (repo, id) = fixture("unstage");
        stage_at(&repo, &id).unwrap();
        let msg = unstage_at(&repo, &id).unwrap();
        assert!(msg.contains("back as it was"), "{msg}");
        assert_eq!(status(&repo, &id), Status::Reviewed, "and it can be fixed or staged again");
        // reversing a patch that CREATED files must remove them from the index
        // and from disk — a leftover file is a change you never asked for
        assert_eq!(digest::git(&repo, &["status", "--porcelain"]).unwrap().trim(), "");
        assert!(!repo.join("src/a.js").exists());
        assert!(!repo.join("test/a.test.js").exists());
        // the evidence is untouched, so staging again works
        let run_dir = state::resolve_run_dir(&repo, &id).unwrap();
        assert!(run_dir.join("impl.patch").is_file() && run_dir.join("review.json").is_file());
        assert!(stage_at(&repo, &id).is_ok(), "must be re-stageable");
        std::fs::remove_dir_all(&repo).ok();
    }
}

#[cfg(test)]
mod amend_tests {
    use super::seed_impl;

    /// The amend path: re-running after a spec revision reuses the previous
    /// implementation rather than paying for a cold pass. Every reason it can't
    /// must fall back to cold, never fail — a bad seed is not a bad run.
    #[test]
    fn seeding_falls_back_to_cold_rather_than_failing() {
        let dir = std::env::temp_dir().join(format!("guvnor-seed-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let (run, wt) = (dir.join("run"), dir.join("wt"));
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        crate::digest::git(&wt, &["init", "-q"]).unwrap();
        crate::digest::ensure_baseline_commit(&wt).unwrap();

        // nothing to seed from
        assert!(!seed_impl(&wt, &run, ""));
        std::fs::write(run.join("impl.patch"), "").unwrap();
        assert!(!seed_impl(&wt, &run, ""), "an empty patch is not a seed");

        let patch = "diff --git a/src/a.js b/src/a.js\nnew file mode 100644\n--- /dev/null\n+++ b/src/a.js\n@@ -0,0 +1 @@\n+ok\n";
        std::fs::write(run.join("impl.patch"), patch).unwrap();
        // the new tests now own a file the old implementation created: seeding
        // would make the two patches non-composable and fail as if the
        // implementer had misbehaved, which is a lie about what happened
        assert!(!seed_impl(&wt, &run, patch), "overlap must start cold, not fail");
        // clean seed lands, and the file is really in the tree
        assert!(seed_impl(&wt, &run, "diff --git a/t/x b/t/x\n--- /dev/null\n+++ b/t/x\n"));
        assert_eq!(std::fs::read_to_string(wt.join("src/a.js")).unwrap(), "ok\n");
        // a patch that no longer applies (already applied) also goes cold
        assert!(!seed_impl(&wt, &run, ""));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Claude, Commands, Limits, Paths};

    fn cfg_with(worker: &str, reviewer: &str) -> Config {
        Config {
            commands: Commands { test: "true".into() },
            paths: Paths { tests: vec!["test/".into()], src: vec!["src/".into()] },
            claude: Claude {
                bin: "claude".into(),
                model_planner: "opus".into(),
                model_worker: worker.into(),
                model_reviewer: reviewer.into(),
            },
            limits: Limits::default(),
        }
    }

    #[test]
    fn decorrelation_warning_fires_only_when_seats_match() {
        assert!(decorrelation_warning(&cfg_with("sonnet", "opus")).is_none());
        let w = decorrelation_warning(&cfg_with("sonnet", "sonnet")).unwrap();
        assert!(w.contains("sonnet"));
    }

    #[test]
    fn session_id_is_uuid_v4_shaped() {
        let id = new_session_id().unwrap();
        assert_eq!(id.len(), 36);
        let b = id.as_bytes();
        assert!(b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-');
        assert_eq!(&id[14..15], "4"); // version 4 nibble
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b")); // variant 10xx
        assert_ne!(new_session_id().unwrap(), id); // not constant
    }

    #[test]
    fn a_fix_round_of_only_test_findings_is_a_dead_end() {
        let f = |file: &str| review::Finding {
            severity: review::Severity::Low,
            file: file.into(),
            note: "n".into(),
        };
        let tests = vec!["test/".into()];
        // every finding under the tests prefix, nothing else to do → dead end
        assert!(all_findings_are_tests(&[f("test/readme.test.js")], &tests));
        // one implementation finding is enough to give the lane real work
        assert!(!all_findings_are_tests(&[f("test/a.test.js"), f("src/x.js")], &tests));
        // no findings at all is not a test-only round (an instruction may carry it)
        assert!(!all_findings_are_tests(&[], &tests));
    }
}
