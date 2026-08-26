use guvnor::spec::Spec;
use guvnor::tui::{lane_display, paragraphs, spec_sections, strip_wt_paths};

#[test]
fn lane_display_extracts_text_and_tools() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi\nthere"},{"type":"tool_use","name":"Write","input":{"file_path":"src/a.js"}}]}}"#;
    assert_eq!(lane_display(line), vec!["hi", "there", "→ Write src/a.js"]);
    let bash = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm test"}}]}}"#;
    assert_eq!(lane_display(bash), vec!["→ Bash npm test"]);
    assert!(lane_display(r#"{"type":"system","subtype":"init"}"#).is_empty());
    assert_eq!(lane_display("not json"), vec!["not json"]);
}

#[test]
fn spec_lines_sections_and_styling() {
    let sp = Spec {
        title: "t".into(),
        objective: "obj line".into(),
        files: vec!["src/a.js (new): things".into()],
        interfaces: vec!["src/a.js: function f(x) — does x".into()],
        constraints: vec!["no deps".into()],
        verification: "node --test".into(),
        acceptance_criteria: vec!["works".into(), "still works".into()],
    };
    // the layout draws one box per section and pairs them by index, so the
    // order and the count are load-bearing
    let titles: Vec<String> = spec_sections(&sp).into_iter().map(|s| s.title).collect();
    assert_eq!(
        titles,
        [
            "Objective",
            "Files (1)",
            "Interfaces (1)",
            "Constraints (1)",
            "Verification",
            "Acceptance criteria (2)"
        ]
    );
    // bodies carry the content and never repeat the heading or the title
    let all: String = spec_sections(&sp)
        .iter()
        .flat_map(|s| s.body.iter())
        .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("obj line") && all.contains("no deps"));
    assert!(all.contains(" 2. "), "criteria are numbered");
    assert!(!all.contains("Files (1)"), "the count belongs to the box title");
    assert!(!all.contains("# t"), "title must not repeat in body");
}

#[test]
fn paragraphs_break_up_a_reviewer_slab() {
    let txt = "First point here. Second point here. Third point here. Fourth one. Fifth.";
    let ls = paragraphs(txt);
    let blanks = ls.iter().filter(|l| l.width() == 0).count();
    assert!(blanks >= 2, "one wall of text, no breathing room: {ls:?}");
    assert_ne!(ls.last().unwrap().width(), 0, "no trailing blank line");
    // every word survives the reflow
    let joined: String =
        ls.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join(" ");
    for w in txt.split_whitespace() {
        assert!(joined.contains(w), "lost {w:?}");
    }
    // an abbreviation must not start a new paragraph on its own
    let abbr = paragraphs("Uses e.g. a Map and nothing else changes here now.");
    assert_eq!(abbr.iter().filter(|l| l.width() == 0).count(), 0, "{abbr:?}");
    // the author's own blank lines are respected, not doubled
    let owned = paragraphs("One.\n\nTwo.");
    assert_eq!(owned.iter().filter(|l| l.width() == 0).count(), 1, "{owned:?}");
    assert!(paragraphs("").is_empty());
    // deliberate line breaks survive: the reviewer is asked for one bullet
    // per criterion, and reflowing them into sentences destroys the list
    let bullets = paragraphs("Verdict sentence.\n\n- 1 met: a.js — does the thing.\n- 2 met: b.js — and the other.");
    let text: Vec<String> =
        bullets.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
    assert_eq!(text, ["Verdict sentence.", "", "- 1 met: a.js — does the thing.", "- 2 met: b.js — and the other."]);
}

#[test]
fn strip_wt_paths_relativizes() {
    let s = "at Object.<anonymous> (/home/u/proj/.guvnor/wt/20260819T150955-adding-verif/test/stats.test.js:6:31)";
    assert_eq!(strip_wt_paths(s), "at Object.<anonymous> (test/stats.test.js:6:31)");
    let quoted = "requireStack: [ '/home/u/proj/.guvnor/wt/run-impl/test/stats.test.js' ]";
    assert_eq!(strip_wt_paths(quoted), "requireStack: [ 'test/stats.test.js' ]");
    assert_eq!(strip_wt_paths("no paths here"), "no paths here");
}
