mod casefile;
mod config;
mod digest;
mod events;
mod harness;
mod hookguard;
mod lane;
mod review;
mod spec;
mod state;
mod worktree;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use serde_json::json;
use spec::Spec;
use state::State;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "gaffer", version, about = "Spec-gated feature orchestrator: LLM lanes type, evidence decides, humans hold the gates.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold .gaffer/gaffer.toml in the current repo
    Init,
    /// Draft a five-part spec with the planner lane (G1: edit + approve it)
    Plan {
        title: String,
        /// Extra context for the planner (constraints, pointers)
        #[arg(long, default_value = "")]
        context: String,
    },
    /// Execute an approved spec: tests lane -> red gate -> impl lane -> green gate -> review
    Run {
        id: String,
    },
    /// Print the case file for human review
    Review {
        id: String,
    },
    /// Approve a gate: spec (G1), tests (G2), work (G3)
    Approve {
        id: String,
        #[arg(long)]
        gate: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Reject a run at a gate with a note (records it; run stays inspectable)
    Reject {
        id: String,
        #[arg(long)]
        gate: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Apply patches to the main repo index (staged). You commit.
    Merge {
        id: String,
        /// Allow merge when the verdict is WARNING instead of APPROVED
        #[arg(long)]
        allow_warning: bool,
    },
    /// Internal: PreToolUse guards called by Claude Code hooks
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        which: HookCmd,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    Write,
    Bash,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gaffer: error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Hook { which } => match which {
            HookCmd::Write => hookguard::run_write_guard(),
            HookCmd::Bash => hookguard::run_bash_guard(),
        },
        Cmd::Init => cmd_init(),
        Cmd::Plan { title, context } => cmd_plan(&title, &context),
        Cmd::Run { id } => cmd_run(&id),
        Cmd::Review { id } => cmd_review(&id),
        Cmd::Approve { id, gate, note } => cmd_gate(&id, &gate, &note, true),
        Cmd::Reject { id, gate, note } => cmd_gate(&id, &gate, &note, false),
        Cmd::Merge { id, allow_warning } => cmd_merge(&id, allow_warning),
    }
}

fn cmd_init() -> Result<i32> {
    let dir = std::env::current_dir()?;
    if !dir.join(".git").exists() {
        bail!("run inside a git repository");
    }
    let gaffer_dir = dir.join(".gaffer");
    std::fs::create_dir_all(gaffer_dir.join("runs"))?;
    let cfg = gaffer_dir.join("gaffer.toml");
    if cfg.exists() {
        println!("already initialized: {}", cfg.display());
        return Ok(0);
    }
    std::fs::write(&cfg, config::CONFIG_TEMPLATE)?;
    println!("wrote {} — edit commands.test and paths, then `gaffer plan \"...\"`", cfg.display());
    Ok(0)
}

fn cmd_plan(title: &str, context: &str) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let id = format!(
        "{}-{}",
        events::now_iso().replace([':', '-'], "").trim_end_matches('Z').to_string(),
        state::slugify(title, 24)
    );
    let run_dir = state::runs_root(&repo).join(&id);
    std::fs::create_dir_all(&run_dir)?;
    let log = events::EventLog::new(&run_dir);
    log.append("plan_started", json!({"title": title}))?;

    println!("planning '{title}' with {} ...", cfg.claude.model_planner);
    let result = lane::run(lane::LaneSpec {
        name: "planner",
        cwd: &repo,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_planner,
        prompt: lane::planner_prompt(title, context, &cfg.commands.test),
        allowed_tools: "Read,Glob,Grep",
        timeout: Duration::from_secs(cfg.limits.lane_timeout_secs),
        transcript: run_dir.join("lanes-planner.ndjson"),
    })?;
    if result.timed_out {
        bail!("planner timed out");
    }
    let json_str = spec::extract_json_object(&result.result_text)
        .context("planner returned no JSON spec")?;
    let mut sp: Spec = serde_json::from_str(json_str).context("planner spec JSON invalid")?;
    if sp.verification.trim().is_empty() {
        sp.verification = cfg.commands.test.clone();
    }
    sp.validate()?;
    std::fs::write(run_dir.join("spec.json"), serde_json::to_string_pretty(&sp)?)?;
    State::new(&id, title).save(&run_dir)?;
    log.append("plan_drafted", json!({"spec_files": sp.files}))?;

    println!("spec draft: {}", run_dir.join("spec.json").display());
    println!("G1: edit it, then:  gaffer approve {id} --gate spec");
    Ok(0)
}

fn cmd_run(id: &str) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let cfg = Config::load(&repo)?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    let sp = Spec::load(&run_dir.join("spec.json"))?;
    if !st.gates.spec.approved {
        bail!("spec not approved (G1). Review {} then `gaffer approve {} --gate spec`",
            run_dir.join("spec.json").display(), st.id);
    }
    let spec_bytes = std::fs::read(run_dir.join("spec.json"))?;
    if digest::sha256_hex(&spec_bytes) != st.gates.spec.sha256 {
        bail!("spec.json changed since approval — re-approve: gaffer approve {} --gate spec", st.id);
    }
    let log = events::EventLog::new(&run_dir);
    let test_cmd = &sp.verification;
    let timeout = Duration::from_secs(cfg.limits.lane_timeout_secs);

    // Worktrees: verification tree + one per writer lane. Cleaned on success,
    // kept for inspection on failure.
    let wt_verif = worktree::create(&repo, &st.id, "verif")?;
    let fail = |st: &mut State, log: &events::EventLog, why: &str, detail: String| -> Result<i32> {
        st.status = format!("failed:{why}");
        st.save(&run_dir)?;
        log.append("run_failed", json!({"why": why, "detail": detail}))?;
        eprintln!("gaffer: run failed [{why}]: {detail}");
        eprintln!("artifacts kept in {} and worktrees for inspection", run_dir.display());
        Ok(2)
    };

    // Gate 0: baseline must be green, else red proves nothing.
    println!("[0/5] baseline check: {test_cmd}");
    let base = harness::run_tests(&wt_verif, test_cmd)?;
    log.append("baseline", json!({"green": base.green, "exit": base.exit_code}))?;
    if !base.green {
        return fail(&mut st, &log, "vacuous_baseline", base.tail);
    }

    // Lane 1: test-writer (cold, spec-only, tests paths only).
    println!("[1/5] test-writer lane ({}) ...", cfg.claude.model_worker);
    let wt_tests = worktree::create(&repo, &st.id, "tests")?;
    lane::write_settings(&wt_tests, &cfg.paths.tests)?;
    let before = digest::capture(&wt_tests)?;
    let tres = lane::run(lane::LaneSpec {
        name: "test-writer",
        cwd: &wt_tests,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_worker,
        prompt: lane::testwriter_prompt(&sp.render(), &cfg.paths.tests, test_cmd),
        allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
        timeout,
        transcript: run_dir.join("lanes-tests.ndjson"),
    })?;
    let after = digest::capture(&wt_tests)?;
    let changed = digest::verdict(&before, &after)?;
    log.append("lane_tests", json!({"changed": changed, "denials": tres.denials, "timed_out": tres.timed_out, "exit": tres.exit_code, "secs": tres.duration_secs}))?;
    if tres.timed_out {
        return fail(&mut st, &log, "tests_lane_timeout", String::new());
    }
    if !changed {
        return fail(&mut st, &log, "tests_lane_noop", "silent no-op: narration without edits".into());
    }
    let tests_patch = worktree::capture_patch(&wt_tests)?;
    if let Err(e) = worktree::validate_patch_within(&tests_patch, &cfg.paths.tests, "tests") {
        return fail(&mut st, &log, "tests_forbidden_paths", format!("{e:#}"));
    }
    std::fs::write(run_dir.join("tests.patch"), &tests_patch)?;
    st.tests_patch_sha256 = digest::sha256_hex(tests_patch.as_bytes());

    // Gate 2 (red): tests must FAIL on base.
    println!("[2/5] red gate: tests must fail on base");
    worktree::apply_patch(&wt_verif, &tests_patch)?;
    let red = harness::run_tests(&wt_verif, test_cmd)?;
    log.append("red_gate", json!({"green": red.green, "exit": red.exit_code}))?;
    if red.green {
        return fail(&mut st, &log, "vacuous_tests", "tests pass without any implementation".into());
    }
    st.red_reason = red.tail.clone();
    st.status = "red_ok".into();
    st.save(&run_dir)?;

    // Lane 2: implementer (cold, spec-only, src paths only, never sees tests).
    println!("[3/5] implementer lane ({}) ...", cfg.claude.model_worker);
    let wt_impl = worktree::create(&repo, &st.id, "impl")?;
    lane::write_settings(&wt_impl, &cfg.paths.src)?;
    let before = digest::capture(&wt_impl)?;
    let ires = lane::run(lane::LaneSpec {
        name: "implementer",
        cwd: &wt_impl,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_worker,
        prompt: lane::implementer_prompt(&sp.render(), &cfg.paths.src, test_cmd),
        allowed_tools: "Read,Glob,Grep,Write,Edit,Bash",
        timeout,
        transcript: run_dir.join("lanes-impl.ndjson"),
    })?;
    let after = digest::capture(&wt_impl)?;
    let changed = digest::verdict(&before, &after)?;
    log.append("lane_impl", json!({"changed": changed, "denials": ires.denials, "timed_out": ires.timed_out, "exit": ires.exit_code, "secs": ires.duration_secs}))?;
    if ires.timed_out {
        return fail(&mut st, &log, "impl_lane_timeout", String::new());
    }
    if !changed {
        return fail(&mut st, &log, "impl_lane_noop", "silent no-op: narration without edits".into());
    }
    let impl_patch = worktree::capture_patch(&wt_impl)?;
    if let Err(e) = worktree::validate_patch_within(&impl_patch, &cfg.paths.src, "impl") {
        return fail(&mut st, &log, "impl_forbidden_paths", format!("{e:#}"));
    }
    std::fs::write(run_dir.join("impl.patch"), &impl_patch)?;
    st.impl_patch_sha256 = digest::sha256_hex(impl_patch.as_bytes());

    // Gate 4 (green): tests must PASS with the implementation.
    println!("[4/5] green gate: tests must pass with implementation");
    worktree::apply_patch(&wt_verif, &impl_patch)?;
    let green = harness::run_tests(&wt_verif, test_cmd)?;
    log.append("green_gate", json!({"green": green.green, "exit": green.exit_code}))?;
    std::fs::write(run_dir.join("green.txt"), &green.tail)?;
    if !green.green {
        return fail(&mut st, &log, "impl_does_not_satisfy_tests", green.tail);
    }
    st.status = "green_ok".into();
    st.save(&run_dir)?;

    // Lane 3: cold reviewer over the combined diff, in the final tree.
    println!("[5/5] reviewer lane ({}) ...", cfg.claude.model_reviewer);
    let combined = format!("{tests_patch}\n{impl_patch}");
    let rres = lane::run(lane::LaneSpec {
        name: "reviewer",
        cwd: &wt_verif,
        claude_bin: &cfg.claude.bin,
        model: &cfg.claude.model_reviewer,
        prompt: lane::reviewer_prompt(&sp.render(), &combined),
        allowed_tools: "Read,Glob,Grep",
        timeout,
        transcript: run_dir.join("lanes-review.ndjson"),
    })?;
    if rres.timed_out {
        return fail(&mut st, &log, "review_timeout", String::new());
    }
    let verdict = match review::parse_verdict(&rres.result_text) {
        Ok(v) => v,
        Err(e) => return fail(&mut st, &log, "review_unparseable", format!("{e:#}")),
    };
    let review = review::Review {
        verdict,
        diff_sha256: digest::sha256_hex(combined.as_bytes()),
        model: cfg.claude.model_reviewer.clone(),
        ts: events::now_iso(),
    };
    std::fs::write(run_dir.join("review.json"), serde_json::to_string_pretty(&review)?)?;
    log.append("reviewed", json!({"verdict": review.verdict.verdict}))?;
    st.status = "reviewed".into();
    st.save(&run_dir)?;

    // Success: throwaway trees go away; patches + evidence remain.
    for wt in [&wt_tests, &wt_impl, &wt_verif] {
        worktree::remove(&repo, wt)?;
    }
    println!("\nverdict: {} — case file:\n  gaffer review {}", review.verdict.verdict, st.id);
    println!("then:  gaffer approve {} --gate tests   (G2)", st.id);
    println!("       gaffer approve {} --gate work    (G3)", st.id);
    println!("       gaffer merge {}", st.id);
    Ok(0)
}

fn cmd_review(id: &str) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    print!("{}", casefile::render(&run_dir)?);
    Ok(0)
}

fn cmd_gate(id: &str, gate: &str, note: &str, approve: bool) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let mut st = State::load(&run_dir)?;
    let slot = match gate {
        "spec" => &mut st.gates.spec,
        "tests" => &mut st.gates.tests,
        "work" => &mut st.gates.work,
        g => bail!("unknown gate '{g}' (spec|tests|work)"),
    };
    slot.approved = approve;
    slot.ts = events::now_iso();
    slot.note = note.to_string();
    if approve && gate == "spec" {
        // Re-validate after human edits and bind the approval to this exact content.
        Spec::load(&run_dir.join("spec.json"))?;
        let bytes = std::fs::read(run_dir.join("spec.json"))?;
        st.gates.spec.sha256 = digest::sha256_hex(&bytes);
        st.status = "spec_approved".into();
    }
    if !approve {
        st.status = format!("failed:rejected_{gate}");
    }
    st.save(&run_dir)?;
    events::EventLog::new(&run_dir).append(
        if approve { "gate_approved" } else { "gate_rejected" },
        json!({"gate": gate, "note": note}),
    )?;
    println!("{} {} gate for {}", if approve { "approved" } else { "rejected" }, gate, st.id);
    Ok(0)
}

fn cmd_merge(id: &str, allow_warning: bool) -> Result<i32> {
    let repo = config::find_repo_root()?;
    let run_dir = state::resolve_run_dir(&repo, id)?;
    let st = State::load(&run_dir)?;

    for (name, a) in [("spec", &st.gates.spec), ("tests", &st.gates.tests), ("work", &st.gates.work)] {
        if !a.approved {
            bail!("gate '{name}' not approved — `gaffer review {}` first", st.id);
        }
    }
    let review: review::Review =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("review.json"))?)
            .context("review.json missing/invalid — run not reviewed")?;
    match review.verdict.verdict.as_str() {
        "APPROVED" => {}
        "WARNING" if allow_warning => {}
        v => bail!("verdict is {v}; merge refused (use --allow-warning for WARNING)"),
    }
    let tests_patch = std::fs::read_to_string(run_dir.join("tests.patch"))?;
    let impl_patch = std::fs::read_to_string(run_dir.join("impl.patch"))?;
    let combined = format!("{tests_patch}\n{impl_patch}");
    if digest::sha256_hex(combined.as_bytes()) != review.diff_sha256 {
        bail!("patches on disk do not match the reviewed diff digest — stale or tampered; re-run");
    }
    let status = digest::git(&repo, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        bail!("main repo tree is dirty; commit or stash first");
    }
    worktree::apply_patch_staged(&repo, &tests_patch)?;
    worktree::apply_patch_staged(&repo, &impl_patch)?;
    let mut st = st;
    st.status = "merged".into();
    st.save(&run_dir)?;
    events::EventLog::new(&run_dir).append("merged", json!({"staged": true}))?;
    println!("staged. Inspect with `git diff --cached`, then commit yourself.");
    Ok(0)
}
