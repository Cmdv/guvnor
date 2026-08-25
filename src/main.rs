mod casefile;
mod config;
mod digest;
mod engine;
mod events;
mod harness;
mod hookguard;
mod lane;
mod review;
mod spec;
mod state;
mod tui;
mod worktree;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use engine::Progress;
use state::Gate;
use std::sync::mpsc::Sender;

#[derive(Parser)]
#[command(name = "guvnor", version, about = "Guv'nor — spec-gated feature orchestrator: LLM lanes type, evidence decides, humans hold the gates.")]
struct Cli {
    /// No subcommand = the TUI; verbs below for scripting
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Show live lane output (TUI starts in verbose; CLI prints lane lines)
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold .guvnor/guvnor.toml in the current repo
    Init,
    /// Draft a five-part spec with the planner lane (then edit + approve it)
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
    /// Approve a gate: spec, tests, or work
    Approve {
        id: String,
        #[arg(long)]
        gate: Gate,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Reject a run at a gate with a note (records it; run stays inspectable)
    Reject {
        id: String,
        #[arg(long)]
        gate: Gate,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Apply a run's patches to your working tree and index, and stop there:
    /// read them, run them, then commit or unstage
    Stage {
        id: String,
    },
    /// Reverse-apply a staged run, leaving your tree as it was. Artifacts stay.
    Unstage {
        id: String,
    },
    /// Commit the staged change. Stages it first if you haven't. Never pushes.
    Commit {
        id: String,
        /// Commit message. Subject on the first line, body after a blank line.
        /// Omit to stage only.
        #[arg(short, long, default_value = "")]
        message: String,
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
    Read,
    Bash,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("guvnor: error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    let verbose = cli.verbose;
    let Some(cmd) = cli.cmd else {
        return tui::run(verbose);
    };
    match cmd {
        Cmd::Hook { which } => match which {
            HookCmd::Write => hookguard::run_write_guard(),
            HookCmd::Read => hookguard::run_read_guard(),
            HookCmd::Bash => hookguard::run_bash_guard(),
        },
        Cmd::Init => cmd_init(),
        Cmd::Plan { title, context } => run_op(verbose, move |tx| engine::plan(&title, &context, tx)),
        Cmd::Run { id } => run_op(verbose, move |tx| engine::run(&id, tx)),
        Cmd::Review { id } => {
            let repo = config::find_repo_root()?;
            let run_dir = state::resolve_run_dir(&repo, &id)?;
            print!("{}", casefile::render(&run_dir)?);
            Ok(0)
        }
        Cmd::Approve { id, gate, note } => {
            println!("{}", engine::set_gate(&id, gate, &note, true)?);
            Ok(0)
        }
        Cmd::Reject { id, gate, note } => {
            println!("{}", engine::set_gate(&id, gate, &note, false)?);
            Ok(0)
        }
        Cmd::Stage { id } => {
            println!("{}", engine::stage(&id)?);
            Ok(0)
        }
        Cmd::Unstage { id } => {
            println!("{}", engine::unstage(&id)?);
            Ok(0)
        }
        Cmd::Commit { id, message } => {
            let (subject, body) = engine::split_commit_message(&message);
            println!("{}", engine::commit(&id, subject, body)?);
            Ok(0)
        }
    }
}

/// Run a long engine op on a background thread, printing its Progress events
/// as they arrive — the same wiring the TUI will use with a render loop.
fn run_op<F>(verbose: bool, op: F) -> Result<i32>
where
    F: FnOnce(&Sender<Progress>) -> Result<i32> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || op(&tx));
    for p in rx {
        print_progress(&p, verbose);
    }
    handle.join().map_err(|_| anyhow!("engine thread panicked"))?
}

fn print_progress(p: &Progress, verbose: bool) {
    match p {
        Progress::Stage(s) => println!("{s}"),
        Progress::RunCreated { .. } => {}
        // CLI stays quiet by default: full transcripts land on disk per lane.
        Progress::LaneLine { lane, line } => {
            if verbose {
                println!("[{lane}] {line}");
            }
        }
        // Gate outcomes surface via Stage/Failed; the event exists for the TUI checklist.
        Progress::GateResult { .. } => {}
        Progress::Done(m) => println!("{m}"),
        Progress::Failed { why, detail } => eprintln!("guvnor: run failed [{why}]: {detail}"),
    }
}

fn cmd_init() -> Result<i32> {
    let dir = std::env::current_dir()?;
    let existed = dir.join(".guvnor/guvnor.toml").exists();
    let cfg = config::init_repo(&dir)?;
    if existed {
        println!("already initialized: {}", cfg.display());
    } else {
        println!("wrote {} — edit commands.test and paths, then `guvnor plan \"...\"`", cfg.display());
    }
    Ok(0)
}
