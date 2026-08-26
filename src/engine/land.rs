use super::*;

pub fn set_gate(id: &str, gate: state::Gate, note: &str, approve: bool) -> Result<String> {
    set_gate_at(&config::find_repo_root()?, id, gate, note, approve)
}

pub fn set_gate_at(
    repo: &Path,
    id: &str,
    gate: state::Gate,
    note: &str,
    approve: bool,
) -> Result<String> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if approve {
        if gate == state::Gate::Spec {
            Spec::load(&run_dir.join("spec.json"))?; // re-validate after human edits
        }
        // Bind the approval to the bytes on screen. Nothing to hash means there
        // is nothing to approve yet.
        let bytes = std::fs::read(run_dir.join(gate.artifact()))
            .with_context(|| format!("{} missing — nothing to approve yet", gate.artifact()))?;
        st.gates.slot_mut(gate).sha256 = digest::sha256_hex(&bytes);
    }
    let slot = st.gates.slot_mut(gate);
    slot.approved = approve;
    slot.ts = events::now_iso();
    slot.note = note.to_string();
    // A gate records the human's decision; where the run is in its life is not
    // the gate's to rewrite. Overwriting `Staged` would leave guvnor's patches
    // in the tree with no `unstage` willing to own them, and overwriting
    // `Committed` would erase the record of a commit that exists.
    let landed = matches!(st.status, Status::Staged | Status::Committed);
    if approve && gate == state::Gate::Spec {
        if !landed {
            st.status = Status::SpecApproved;
        }
        // Spec accepted — close the iterating planner session.
        st.planner_session_id.clear();
    }
    if !approve && !landed {
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
    // Untracked files are ignored, same as `write-tree` in `index_tree`: a scratch
    // file that isn't part of any patch can't collide with one, so it can't dirty
    // the tree for our purposes.
    let status = git::git(repo, &["status", "--porcelain", "--untracked-files=no"])?;
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
    // Each of these two gates approves one patch. Without the digest a re-run's
    // fresh patches inherit yesterday's ticks: the run writes a new review.json
    // as well, so the combined check below would pass on a diff nobody read.
    for gate in [state::Gate::Tests, state::Gate::Work] {
        let bytes = std::fs::read(run_dir.join(gate.artifact()))
            .with_context(|| format!("{} missing", gate.artifact()))?;
        if digest::sha256_hex(&bytes) != st.gates.slot(gate).sha256 {
            bail!(
                "{} is not what the {} gate approved — read it again and re-approve",
                gate.artifact(),
                gate.as_str()
            );
        }
    }
    let review: review::Review =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("review.json"))?)
            .context("review.json missing/invalid — run not reviewed")?;
    let (tests_patch, impl_patch) = read_patches(run_dir)?;
    // The reviewer's verdict is bound to a digest of the exact bytes it read.
    // Without this an approval could slide onto a different diff.
    let combined = combined(&tests_patch, &impl_patch);
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
    let diff = combined(&tests_patch, &impl_patch);
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
        // No tools at all. The diff and the objective are already in the prompt,
        // so there is nothing to look up, and this is the one lane whose cwd is
        // the developer's real repo rather than a throwaway worktree: with no
        // tools there is no fence to get wrong.
        allowed_tools: "",
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

pub fn stage_at(repo: &Path, id: &str) -> Result<String> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if staging_intact(repo, &st)? {
        return Ok("already staged — `git diff --cached` to read it".into());
    }
    // Staged, but the index no longer hashes to what we put there. If the tree
    // came back clean you reset it away, and staging afresh is exactly right;
    // anything still in there is yours now, and not ours to overwrite.
    if st.status == Status::Staged
        && !git::git(repo, &["status", "--porcelain", "--untracked-files=no"])?
            .trim()
            .is_empty()
    {
        bail!("{DRIFTED}");
    }
    let (tests_patch, impl_patch) = commit_checks(repo, &run_dir, &st)?;
    // One invocation, because `git apply` is atomic per invocation: applied
    // separately, a second patch that fails leaves the first in your tree with
    // status still `Reviewed` and `staged_tree` empty, so no verb owns it. The
    // concatenation is the same byte string the approval digest binds to.
    worktree::apply_patch_staged(repo, &combined(&tests_patch, &impl_patch))?;
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

pub fn unstage_at(repo: &Path, id: &str) -> Result<String> {
    let run_dir = state::resolve_run_dir(repo, id)?;
    let mut st = State::load(&run_dir)?;
    if st.status != Status::Staged {
        bail!("nothing staged for this run");
    }
    if !staging_intact(repo, &st)? {
        bail!("{DRIFTED}");
    }
    let (tests_patch, impl_patch) = read_patches(&run_dir)?;
    // One invocation, same reason as `stage_at`: a half-reversed tree is a state
    // where all three verbs refuse. Hunk order does not matter here because the
    // two patches are checked for overlap before either is ever kept, so they
    // touch disjoint files.
    worktree::reverse_patch_staged(repo, &combined(&tests_patch, &impl_patch))?;
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

pub fn commit_at(repo: &Path, id: &str, subject: &str, body: &str) -> Result<String> {
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

pub fn unstaged_edits_at(repo: &Path, id: &str) -> Result<Vec<String>> {
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

