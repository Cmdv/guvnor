use super::*;

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

/// Put the previous run's implementation into the implementer's tree, so a
/// re-run after a spec revision adapts it instead of writing it again from
/// nothing. Returns whether it landed; every reason it might not is a reason to
/// start cold, never to fail.
///
/// Unconditional: no flag disables it. A spec change big enough that the old
/// implementation actively misleads costs one rework round to discover, and
/// the bounded rework loop already exists to absorb exactly that.
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

/// No lane spends a token against a spec no human has accepted, and none spends
/// one against a spec edited since. Both the initial run and a fix round go
/// through here, because a `replan` resets the gate and a fix round would
/// otherwise implement against the spec that replacement threw away.
fn require_approved_spec(run_dir: &Path, st: &State) -> Result<()> {
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
    Ok(())
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
    require_approved_spec(&run_dir, &st)?;
    let log = events::EventLog::new(&run_dir);
    // This run will overwrite both patches, so any tick on them was for a diff
    // that is about to stop existing. `approval_checks` would catch it at the
    // commit anyway; clearing here is what keeps the run screen honest in the
    // meantime. A run that dies before writing a patch loses a tick it could
    // have kept, which costs one re-approval on a run being redone regardless.
    if st.gates.tests.approved || st.gates.work.approved {
        st.gates.tests = Default::default();
        st.gates.work = Default::default();
        st.save(&run_dir)?;
        log.append(
            "gate_reset",
            json!({"gate": "tests,work", "why": "re-run — the lanes write fresh patches"}),
        )?;
    }
    // The human's configured command, never `sp.verification`. That field is
    // written by the planner lane and lands in `sh -c` with no guard between,
    // so a planner that read an injected file could append `git push` to it and
    // guvnor would run it three times a run. It stays in the spec as prompt
    // text, where a lane reads it and nothing executes it.
    let test_cmd = &cfg.commands.test;
    let timeout = Duration::from_secs(cfg.limits.lane_timeout_secs);

    // Keep the worktree container (.guvnor/wt/) out of git before we touch it.
    worktree::ensure_wt_ignored(&repo)?;

    // A fresh `git init` has no HEAD; worktrees and the evidence digests all
    // need a baseline tree. Bootstrap one from the current tree so a brand-new
    // repo can run (the human still owns every later commit).
    if git::ensure_baseline_commit(&repo)? {
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
    let base = match harness::run_tests(&wt_verif, test_cmd, timeout) {
        Ok(o) => o,
        Err(e) => return fail(&run_dir, &mut st, &log, tx, "baseline_unrunnable", format!("{e:#}")),
    };
    log.append("baseline", json!({"green": base.green, "exit": base.exit_code}))?;
    let _ = tx.send(Progress::GateResult {
        gate: "baseline".into(),
        ok: base.green,
        detail: if base.green { String::new() } else { base.tail.clone() },
    });
    if base.timed_out {
        return fail(&run_dir, &mut st, &log, tx, "baseline_timeout", base.tail);
    }
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
    // From here the artifacts on disk belong to this spec, and a later replan
    // is detectable by comparing this against spec.json.
    st.spec_sha_at_run = st.gates.spec.sha256.clone();

    // Gate 2 (red): tests must FAIL on base.
    let _ = tx.send(Progress::Stage("[2/5] red gate: tests must fail on base".into()));
    if let Err(e) = worktree::apply_patch(&wt_verif, &tests_patch) {
        return fail(&run_dir, &mut st, &log, tx, "verif_apply_failed", format!("{e:#}"));
    }
    let red = match harness::run_tests(&wt_verif, test_cmd, timeout) {
        Ok(o) => o,
        Err(e) => return fail(&run_dir, &mut st, &log, tx, "red_gate_unrunnable", format!("{e:#}")),
    };
    log.append("red_gate", json!({"green": red.green, "exit": red.exit_code}))?;
    let _ = tx.send(Progress::GateResult {
        gate: "red".into(),
        ok: !red.green,
        detail: if red.green { "tests pass without any implementation".into() } else { String::new() },
    });
    // A hang is not a red. The gate asks whether the tests fail *because the
    // feature is missing*, and a suite that never finished has not answered.
    if red.timed_out {
        return fail(&run_dir, &mut st, &log, tx, "red_gate_timeout", red.tail);
    }
    if red.green {
        return fail(&run_dir, &mut st, &log, tx, "vacuous_tests", "tests pass without any implementation".into());
    }
    st.red_reason = red.tail;
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
        if let Err(e) = worktree::apply_patch(&wt_verif, &impl_patch) {
            return fail(&run_dir, &mut st, &log, tx, "verif_apply_failed", format!("{e:#}"));
        }
        let green = match harness::run_tests(&wt_verif, test_cmd, timeout) {
            Ok(o) => o,
            Err(e) => {
                return fail(&run_dir, &mut st, &log, tx, "green_gate_unrunnable", format!("{e:#}"))
            }
        };
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
        impl_patch = p;
        // verif tree back to base + tests before re-applying the cumulative patch
        if let Err(e) = worktree::reset_hard(&wt_verif)
            .and_then(|()| worktree::apply_patch(&wt_verif, &tests_patch))
        {
            return fail(&run_dir, &mut st, &log, tx, "verif_apply_failed", format!("{e:#}"));
        }
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
    let combined = combined(tests_patch, impl_patch);
    // The reviewer has no shell (a claim to have run tests proves nothing — the
    // green gate already ran them). Hand it the gate's own output, written to
    // green.txt by both callers immediately above, or it cannot judge a
    // "tests pass" criterion and will file its denied Bash as a finding.
    let green = std::fs::read_to_string(run_dir.join("green.txt")).unwrap_or_default();
    // The verif tree has never had settings written to it, so without this the
    // reviewer runs with no read fence and loads whatever .claude/settings.json
    // it happens to find. Empty deny list: it reads the whole tree by design.
    lane::write_settings(wt_verif, &[])?;
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
    require_approved_spec(&run_dir, &st)?;
    let log = events::EventLog::new(&run_dir);
    let test_cmd = &cfg.commands.test;
    let timeout = Duration::from_secs(cfg.limits.lane_timeout_secs);

    let (tests_patch, impl_patch) = read_patches(&run_dir).context("nothing to fix")?;
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
    // output back and let it try again, bounded by the same budget. Without
    // this the human would have to read the failure and re-type it into the
    // instruction box — evidence the engine already had on disk.
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
        let green = harness::run_tests(&wt_verif, test_cmd, timeout)?;
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
    // The approved work no longer exists — that verdict was for the old diff.
    st.gates.work = Default::default();
    st.status = Status::GreenOk;
    // Dealt with: don't ask about these again. If the fresh review re-raises
    // one, the UI shows it as re-raised rather than pretending it's resolved.
    let known: Vec<String> = st.fixed_findings.iter().map(state::finding_key).collect();
    st.fixed_findings
        .extend(findings.iter().filter(|f| !known.contains(&state::finding_key(f))).cloned());
    st.save(&run_dir)?;
    log.append("gate_reset", json!({"gate": "work", "why": "impl changed by fix round"}))?;

    review_and_finish(&cfg, &repo, &run_dir, &mut st, &sp, &wt_verif, &tests_patch, &new_impl, tx)
}

/// Every `fix_broke_tests` says this: the attempt is gone and the implementation
/// that passed is still the one on disk. Stated once so the failure and the
/// advice cannot drift apart about what state the run is in.
const BROKE_TAIL: &str =
    "the fix regressed the suite; it was thrown away and impl.patch on disk is unchanged";

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
        crate::git::git(&wt, &["init", "-q"]).unwrap();
        crate::git::ensure_baseline_commit(&wt).unwrap();

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
