use crate::review::Review;
use crate::spec::Spec;
use crate::state::State;
use anyhow::Result;
use std::path::Path;

/// The G2/G3 review surface: everything a human needs to judge a run,
/// assembled from on-disk artifacts. Claims come with evidence attached.
pub fn render(run_dir: &Path) -> Result<String> {
    let state = State::load(run_dir)?;
    let mut s = String::new();
    s.push_str(&format!("# Run {} — {}\nstatus: {}\n", state.id, state.title, state.status));

    if let Ok(spec) = Spec::load(&run_dir.join("spec.json")) {
        s.push_str("\n## Spec\n");
        s.push_str(&spec.render());
    }

    for (label, file, sha) in [
        ("Tests patch", "tests.patch", &state.tests_patch_sha256),
        ("Implementation patch", "impl.patch", &state.impl_patch_sha256),
    ] {
        if let Ok(patch) = std::fs::read_to_string(run_dir.join(file)) {
            let files = crate::worktree::patch_paths(&patch);
            let added = patch.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count();
            let removed = patch.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count();
            s.push_str(&format!(
                "\n## {label}\nsha256: {sha}\nfiles: {files:?}\n+{added} -{removed} lines ({file})\n"
            ));
        }
    }

    if !state.red_reason.is_empty() {
        s.push_str(&format!(
            "\n## Red evidence (tests failed on base, as required)\n```\n{}\n```\n",
            state.red_reason
        ));
    }
    if let Ok(green) = std::fs::read_to_string(run_dir.join("green.txt")) {
        s.push_str(&format!("\n## Green evidence (tests pass with implementation)\n```\n{green}\n```\n"));
    }

    if let Ok(raw) = std::fs::read_to_string(run_dir.join("review.json")) {
        if let Ok(review) = serde_json::from_str::<Review>(&raw) {
            s.push_str(&format!(
                "\n## Reviewer verdict: {}\n{}\ndiff_sha256: {} (model: {})\n",
                review.verdict.verdict, review.verdict.summary, review.diff_sha256, review.model
            ));
            for f in &review.verdict.findings {
                s.push_str(&format!("- [{}] {} — {}\n", f.severity, f.file, f.note));
            }
        }
    }

    s.push_str(&format!(
        "\n## Gates\nspec:  {}\ntests: {}  (G2 — approve the tests as honest)\nwork:  {}  (G3 — approve the implementation)\n",
        mark(&state.gates.spec),
        mark(&state.gates.tests),
        mark(&state.gates.work)
    ));
    Ok(s)
}

fn mark(a: &crate::state::Approval) -> String {
    if a.approved {
        format!("approved {} {}", a.ts, a.note)
    } else {
        "pending".into()
    }
}
