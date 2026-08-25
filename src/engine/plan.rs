use super::*;
use std::io::Read;

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
        // Persist before the lane runs: the session exists on Claude's side the
        // moment it is created, so a lane that errors would otherwise lose the
        // only handle to it and the next replan would open another cold one.
        st.save(&run_dir)?;
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
            st.save(&run_dir)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
