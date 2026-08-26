//! Engine: the orchestration ops behind the CLI verbs, reused by the TUI.
//! Long ops (plan, run) report via `Progress` over an mpsc sender
//! and return an exit code; short ops (set_gate, commit) return a message.

pub mod land;
pub mod plan;
pub mod run;

pub use land::*;
pub use plan::*;
pub use run::*;

use crate::config::{self, Config};
use crate::spec::{self, Spec};
use crate::state::{self, State, Status};
use crate::{digest, events, git, harness, lane, review, worktree};
use anyhow::{bail, Context, Result};
use serde_json::json;
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
    if let (Some(a), serde_json::Value::Object(b)) = (into.as_object_mut(), from) {
        a.extend(b);
    }
}

/// tests.patch + impl.patch, in the order they apply. Errors name the missing
/// file, so callers only add what their verb means ("nothing to fix").
fn read_patches(run_dir: &Path) -> Result<(String, String)> {
    let read = |name: &str| {
        std::fs::read_to_string(run_dir.join(name)).with_context(|| format!("{name} missing"))
    };
    Ok((read("tests.patch")?, read("impl.patch")?))
}

/// The two patches as one byte string: what the reviewer's verdict is digested
/// over, what the landing re-digests, and what `git apply` receives. All three
/// have to agree byte for byte or an approval binds to something nobody read,
/// so they read it from here rather than each spelling out the same `format!`.
pub fn combined(tests_patch: &str, impl_patch: &str) -> String {
    format!("{tests_patch}\n{impl_patch}")
}
