use super::*;

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
    let status = git::git(repo, &["status", "--porcelain"])?;
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
    let (tests_patch, impl_patch) = read_patches(run_dir)?;
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
    Ok(git::git(repo, &["write-tree"])?.trim().to_string())
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
    let (tests_patch, impl_patch) = read_patches(&run_dir)?;
    let diff = format!("{tests_patch}\n{impl_patch}");
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
    if st.status == Status::Staged && !git::git(repo, &["status", "--porcelain"])?.trim().is_empty()
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
    let (tests_patch, impl_patch) = read_patches(&run_dir)?;
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
/// one-liner `guvnor commit <id> -m "..."` works end to end.
///
/// An empty subject means "stage only". Guv'nor never pushes; that boundary
/// is not configurable.
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
    git::git(repo, &args)
        .context("git commit failed (is user.name/user.email set? did a pre-commit hook refuse?)")?;
    let sha = git::git(repo, &["rev-parse", "--short", "HEAD"])?.trim().to_string();
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
    Ok(git::git(repo, &["status", "--porcelain"])?
        .lines()
        .filter_map(|line| {
            // XY path: Y is the worktree column, ' ' there means "index and tree agree"
            let (worktree_col, path) = (line.chars().nth(1)?, line.get(3..)?.trim());
            (worktree_col != ' ' && owned.iter().any(|f| f == path)).then(|| path.to_string())
        })
        .collect())
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
        let git = |args: &[&str]| git::git(&repo, args).unwrap();
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
            let p = git::git(&repo, &["diff", "--cached"]).unwrap();
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
            git::git(&repo, &["status", "--porcelain"]).unwrap().trim(),
            "",
            "fixture must start clean or the staging checks are untested"
        );
        (repo, id.to_string())
    }

    fn status(repo: &Path, id: &str) -> Status {
        State::load(&state::resolve_run_dir(repo, id).unwrap()).unwrap().status
    }

    /// The reviewer holds no gate. The human's judgement is the work gate, made
    /// with the verdict on the screen next to it; the check that carries the
    /// weight is the diff digest.
    #[test]
    fn the_verdict_does_not_gate_landing() {
        for d in [review::Decision::Warning, review::Decision::Blocked] {
            let (repo, id) = fixture_verdict(&format!("verdict-{d}"), d);
            assert!(stage_at(&repo, &id).is_ok(), "{d} must be landable");
            assert_eq!(status(&repo, &id), Status::Staged);
            std::fs::remove_dir_all(&repo).ok();
        }
        // ...but a diff that is not the reviewed one still cannot land, whatever
        // the verdict said. That is the check that carries the weight.
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
        let log = git::git(&repo, &["log", "--oneline"]).unwrap();
        assert_eq!(log.lines().count(), 1, "staging must not commit: {log}");
        // staging twice is not an error and does not double-apply
        assert!(stage_at(&repo, &id).unwrap().contains("already staged"));

        // then the commit, which is the one guvnor will sign
        let msg = commit_at(&repo, &id, "add a", "why").unwrap();
        assert!(msg.contains("committed"), "{msg}");
        assert_eq!(status(&repo, &id), Status::Committed);
        let log = git::git(&repo, &["log", "--oneline"]).unwrap();
        assert_eq!(log.lines().count(), 2, "exactly one commit: {log}");
        let body = git::git(&repo, &["log", "-1", "--pretty=%B"]).unwrap();
        assert!(body.contains("add a") && body.contains("why"), "{body}");
        assert_eq!(git::git(&repo, &["status", "--porcelain"]).unwrap().trim(), "");
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
        git::git(&repo, &["add", "src/a.js"]).unwrap();

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
        assert_eq!(git::git(&repo, &["log", "--oneline"]).unwrap().lines().count(), 1);
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
        assert_eq!(git::git(&repo, &["status", "--porcelain"]).unwrap().trim(), "");
        assert!(!repo.join("src/a.js").exists());
        assert!(!repo.join("test/a.test.js").exists());
        // the evidence is untouched, so staging again works
        let run_dir = state::resolve_run_dir(&repo, &id).unwrap();
        assert!(run_dir.join("impl.patch").is_file() && run_dir.join("review.json").is_file());
        assert!(stage_at(&repo, &id).is_ok(), "must be re-stageable");
        std::fs::remove_dir_all(&repo).ok();
    }
}
