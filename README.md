```
   ░█▀▀░█░█░█░█░▀░█▀█░█▀█░█▀▄
   ░█░█░█░█░▀▄▀░░░█░█░█░█░█▀▄
   ░▀▀▀░▀▀▀░░▀░░░░▀░▀░▀▀▀░▀░▀
```

# guvnor

Spec-gated feature orchestrator: LLM lanes type, evidence decides, humans hold
the gates. Design lessons come from
[`CharlesHoskinson/foreman`](https://github.com/CharlesHoskinson/foreman)'s
public incident log of a similar loop lying to itself.

## Install

```
curl -fsSL https://raw.githubusercontent.com/Cmdv/guvnor/main/install.sh | sh
```

Grabs the right binary for your OS/arch from the [releases
page](https://github.com/Cmdv/guvnor/releases) (macOS + Linux, `x86_64` and
`aarch64`) and drops it in `/usr/local/bin`. Building from source works too:
`cargo install --path .`.

## Before your first run

- a git repo with a passing test suite and one command to run it (`npm test`,
  `pytest -q`, `cargo test`, `cabal test`, …)
- the `claude` CLI on your PATH — guvnor drives it headlessly to do the typing

## Your first run

guvnor is cwd-based: `cd` into the repo and run `guvnor` with no arguments.
That opens the TUI — there's no `--repo` flag and nothing else to learn to
get started.

### 1. Configure the repo (once)

No `.guvnor/guvnor.toml` yet? The home screen says so — press `c`. Pick a
language preset (node/rust/python/haskell/other) to fill in the test command
and paths, and a model preset (max/balanced/budget) for the three seats —
planner, worker, reviewer — or type your own values for any of it. Saving
writes `.guvnor/guvnor.toml`.

<!-- screenshot: home screen, unconfigured, config modal open -->

### 2. Describe the feature

The home screen always has a "new feature" box sitting next to the logo.
Press `n` to focus it, type a title and, if it helps, a paragraph of context
or constraints. `↵` sends it to the planner, which drafts a five-part spec:
Objective, Files, Interfaces, Constraints, Verification.

<!-- screenshot: home screen, new-feature box focused -->

### 3. Read the spec, argue with it

You land on the Spec tab. Not quite right? Press `i`, type feedback, `↵` —
the same planner session redrafts it. Repeat as needed; nothing downstream
exists yet, so there's nothing to invalidate. Happy with it? `↵` approves the
spec. Approval binds to that exact text — edit it later and you'll be asked
to approve it again.

<!-- screenshot: spec tab -->

### 4. Run it

Press `r`. Six stages run one after another, shown live: baseline (your test
command must already be green) → a test-writer lane that sees only the spec
→ a red gate (the new tests must fail before anything exists that could make
them pass) → an implementer lane that also sees only the spec, never the
tests it's being checked against → a green gate (tests must now pass) → a
reviewer that reads the diff and the green gate's own output, with no shell
of its own.

<!-- screenshot: progress screen, mid-run -->

### 5. Read Tests and Work

Two tabs unlock as the evidence lands. Each is a file list, not a wall of
diff: `↑↓` moves, `space` opens a file in place. `↵` on the tab you just
read judges it.

<!-- screenshot: work tab, a file expanded -->

### 6. Triage the review

The reviewer leaves a verdict — APPROVED, WARNING, or BLOCKED — plus a list
of findings by severity. Tick the ones worth fixing and `↵`: a fix lane
patches only those, the green gate re-checks, and the reviewer looks again.
Nothing left worth fixing, or happy to ship as-is? Approve the tab like the
other two.

<!-- screenshot: review tab, findings ticked -->

### 7. Stage, then commit

With Spec, Tests, and Work all approved, press `s` to open the stage box at
the foot of the Review tab. Staging applies both patches to your real working
tree and stops there — open the files, run the app, `git diff --cached` it
yourself. From there: commit (guvnor writes the message and the commit — it
never pushes) or unstage (backs out cleanly; every artifact stays on disk
either way).

<!-- screenshot: stage box + commit modal -->

That's a run, start to finish.

## Scripting it

Same loop, one CLI verb per step — `guvnor --help` lists them all:

```
guvnor plan "feature title" --context "..."
guvnor approve <id> --gate spec
guvnor run <id>
guvnor review <id>                 # prints the case file to your terminal
guvnor approve <id> --gate tests
guvnor approve <id> --gate work
guvnor stage <id>
guvnor commit <id> -m "message"    # or: guvnor unstage <id>
```
