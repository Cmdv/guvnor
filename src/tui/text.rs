//! Artifacts to styled lines: specs, diffs, lane chatter, test failures.
//! Pure functions over strings — no layout, no state.

use crate::spec::Spec;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Break reviewer prose into readable paragraphs. Models emit one long run of
/// sentences; the terminal then wraps it into an unreadable slab. Split on
/// sentence ends and group a few per paragraph, with a blank line between, so
/// there is somewhere for the eye to rest. Widget wrapping does the rest.
pub fn paragraphs(text: &str) -> Vec<Line<'static>> {
    const PER_PARA: usize = 2;
    let mut out: Vec<Line> = Vec::new();
    // respect the author's own breaks first; only reflow blocks that lack them
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        // A single newline is a line the writer meant — the reviewer is asked
        // for one bullet per criterion, and reflowing them back into a slab is
        // exactly the wall of text this function exists to prevent.
        if block.contains('\n') {
            if !out.is_empty() {
                out.push(Line::raw(""));
            }
            out.extend(block.lines().map(|l| Line::raw(l.trim_end().to_string())));
            continue;
        }
        let flat = block.split_whitespace().collect::<Vec<_>>().join(" ");
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        let mut sentence = String::new();
        let mut count = 0;
        for word in flat.split(' ') {
            if !sentence.is_empty() {
                sentence.push(' ');
            }
            sentence.push_str(word);
            // "e.g." / "src/a.js." shouldn't split: require the next word to
            // start a new sentence, which we approximate by length
            let ends = word.ends_with('.') || word.ends_with('!') || word.ends_with('?');
            if ends && word.len() > 2 {
                count += 1;
                if count >= PER_PARA {
                    out.push(Line::raw(std::mem::take(&mut sentence)));
                    out.push(Line::raw(""));
                    count = 0;
                }
            }
        }
        if !sentence.trim().is_empty() {
            out.push(Line::raw(sentence));
        }
        while out.last().is_some_and(|l| l.width() == 0) {
            out.pop();
        }
    }
    out
}

// ---- spec rendering: styled sections straight from the struct --------------

/// One titled part of a spec. The screen draws a box per section; the narrow
/// fallback concatenates them under rules. One source, so the two can't drift.
pub struct SpecSection {
    pub title: String,
    pub body: Vec<Line<'static>>,
}

fn spec_file_line(f: &str) -> Line<'static> {
    let mut spans = vec![Span::styled("‣ ", Style::new().fg(Color::DarkGray))];
    let (head, desc) = match f.split_once(':') {
        Some((h, d)) => (h.to_string(), Some(d.to_string())),
        None => (f.to_string(), None),
    };
    // "path (new)" / "path (modified)" markers
    if let Some(i) = head.find(" (") {
        let (path, marker) = head.split_at(i);
        let mstyle = if marker.contains("new") {
            Style::new().fg(Color::Green)
        } else {
            Style::new().fg(Color::Yellow)
        };
        spans.push(Span::styled(path.to_string(), Style::new().fg(Color::Cyan)));
        spans.push(Span::styled(marker.to_string(), mstyle));
    } else {
        spans.push(Span::styled(head, Style::new().fg(Color::Cyan)));
    }
    if let Some(d) = desc {
        spans.push(Span::raw(format!(":{d}")));
    }
    Line::from(spans)
}

fn spec_interface_line(i: &str) -> Line<'static> {
    let mut spans = vec![Span::styled("‣ ", Style::new().fg(Color::DarkGray))];
    let rest = match i.split_once(": ") {
        // leading repo path ("src/math/stats.js: ...") — no spaces/parens in it
        Some((path, r)) if path.contains('/') && !path.contains(' ') && !path.contains('(') => {
            spans.push(Span::styled(format!("{path}: "), Style::new().fg(Color::Cyan)));
            r.to_string()
        }
        _ => i.to_string(),
    };
    match rest.split_once(" — ") {
        Some((sig, desc)) => {
            spans.push(Span::styled(sig.to_string(), Style::new().bold()));
            spans.push(Span::styled(format!(" — {desc}"), Style::new().fg(Color::DarkGray)));
        }
        None => spans.push(Span::raw(rest)),
    }
    Line::from(spans)
}

/// How a section's body is spaced. A named type, not two `bool`s — there is no
/// argument order to get wrong.
enum Spacing {
    /// A leading blank under the title, and a blank between items so each entry
    /// (and its wrapped continuation) reads as its own block.
    Loose,
    /// A leading blank under the title, items packed together.
    Packed,
    /// No leading blank — content sits straight under the title.
    Flush,
}

/// Assemble a section body at `spacing`: an optional leading blank, then the
/// items (blank-separated for `Loose`).
fn body(spacing: Spacing, items: impl IntoIterator<Item = Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    if !matches!(spacing, Spacing::Flush) {
        out.push(Line::raw(""));
    }
    for (n, item) in items.into_iter().enumerate() {
        if matches!(spacing, Spacing::Loose) && n > 0 {
            out.push(Line::raw(""));
        }
        out.push(item);
    }
    out
}

/// The spec's six sections, in order: objective · files · interfaces ·
/// constraints · verification · acceptance criteria. Title omitted — the screen
/// header already shows it. The layout depends on this order, so it is fixed.
pub fn spec_sections(sp: &Spec) -> Vec<SpecSection> {
    let dim = Style::new().fg(Color::DarkGray);
    let sec = |title: String, body: Vec<Line<'static>>| SpecSection { title, body };
    vec![
        sec(
            "Objective".into(),
            body(Spacing::Loose, sp.objective.lines().map(|l| Line::raw(l.to_string()))),
        ),
        sec(
            format!("Files ({})", sp.files.len()),
            body(Spacing::Packed, sp.files.iter().map(|f| spec_file_line(f))),
        ),
        sec(
            format!("Interfaces ({})", sp.interfaces.len()),
            body(Spacing::Loose, sp.interfaces.iter().map(|i| spec_interface_line(i))),
        ),
        sec(
            format!("Constraints ({})", sp.constraints.len()),
            body(
                Spacing::Loose,
                sp.constraints
                    .iter()
                    .map(|c| Line::from(vec![Span::styled("‣ ", dim), Span::raw(c.clone())])),
            ),
        ),
        sec(
            "Verification".into(),
            body(
                Spacing::Flush,
                [Line::from(vec![
                    Span::styled("$ ", dim),
                    Span::styled(sp.verification.clone(), Style::new().fg(Color::Green)),
                ])],
            ),
        ),
        sec(
            format!("Acceptance criteria ({})", sp.acceptance_criteria.len()),
            body(Spacing::Packed, sp.acceptance_criteria.iter().enumerate().map(|(n, a)| {
                Line::from(vec![
                    Span::styled(format!("{:>2}. ", n + 1), Style::new().fg(Color::Yellow).bold()),
                    Span::raw(a.clone()),
                ])
            })),
        ),
    ]
}

/// Extract human-relevant lines from a lane's stream-json line for the
/// verbose tail: assistant text, tool invocations, guard blocks.
pub fn lane_display(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return vec![line.to_string()];
    };
    match v["type"].as_str() {
        Some("assistant") => v["message"]["content"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .flat_map(|c| match c["type"].as_str() {
                        Some("text") => c["text"].as_str().unwrap_or("").lines().map(String::from).collect::<Vec<_>>(),
                        Some("tool_use") => {
                            let name = c["name"].as_str().unwrap_or("tool");
                            let input = &c["input"];
                            // the argument that tells you what it's touching
                            let arg = input["file_path"]
                                .as_str()
                                .or_else(|| input["command"].as_str())
                                .or_else(|| input["pattern"].as_str())
                                .or_else(|| input["path"].as_str())
                                .unwrap_or("");
                            let mut a = strip_wt_paths(&arg.replace('\n', " ⏎ "));
                            if a.chars().count() > 72 {
                                a = a.chars().take(71).collect::<String>() + "…";
                            }
                            vec![if a.is_empty() {
                                format!("→ {name}")
                            } else {
                                format!("→ {name} {a}")
                            }]
                        }
                        _ => vec![],
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("user") if line.contains("guvnor: BLOCKED") => vec!["✗ blocked by guard".into()],
        _ => vec![],
    }
}

/// Worktree paths are noise: `/…/.guvnor/wt/<run>-<lane>/rest` → `rest`.
/// The absolute prefix starts at the nearest delimiter before the marker.
pub fn strip_wt_paths(s: &str) -> String {
    const MARK: &str = ".guvnor/wt/";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(hit) = rest.find(MARK) {
        let head = &rest[..hit];
        let start = head
            .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | '\'' | '"' | '[' | '='))
            .map(|i| i + c_len(head, i))
            .unwrap_or(0);
        out.push_str(&rest[..start]);
        let after = &rest[hit + MARK.len()..];
        match after.find('/') {
            Some(slash) => rest = &after[slash + 1..],
            None => rest = after,
        }
    }
    out.push_str(rest);
    out
}

fn c_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

/// Colorize one line of harness/test output on the failure screen: passes
/// green, failures red, counters and stack frames dimmed so the signal pops.
pub fn failure_line(l: &str) -> Line<'static> {
    let t = l.trim_start();
    let style = if t.starts_with('✔') || t.starts_with('✓') || t.starts_with("ok ") {
        Style::new().fg(Color::Green)
    } else if t.starts_with('✖') || t.starts_with('✗') || t.starts_with("not ok") || t.contains("AssertionError") {
        Style::new().fg(Color::Red)
    } else if t.starts_with("at ") || t.starts_with('ℹ') || t.starts_with("i ") {
        Style::new().fg(Color::DarkGray)
    } else if t.starts_with("+ ") {
        Style::new().fg(Color::Green)
    } else if t.starts_with("- ") {
        Style::new().fg(Color::Red)
    } else {
        Style::new()
    };
    Line::styled(format!(" {}", strip_wt_paths(l)), style)
}

/// What the last fix round was told to fix. On a `fix_broke_tests` the conflict
/// between one of these and a test IS the failure, so the evidence is incomplete
/// without them — and the Review tab's tick marks are cleared by the time you
/// read the failure.
pub fn last_ticked(dir: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(dir.join("events.ndjson")) else { return Vec::new() };
    // the last `fix_started` wins, so scan from the tail
    raw.lines()
        .rev()
        .find_map(|l| {
            let v = serde_json::from_str::<serde_json::Value>(l).ok()?;
            (v["event"] == "fix_started").then(|| {
                v["data"]["ticked"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| {
                                let file = f["file"].as_str().unwrap_or("");
                                let note = f["note"].as_str().unwrap_or("");
                                if file.is_empty() { note.into() } else { format!("{file} — {note}") }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default()
}

pub fn last_failure(dir: &std::path::Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(dir.join("events.ndjson")).ok()?;
    // the last `run_failed` wins, so scan from the tail
    raw.lines().rev().find_map(|l| {
        let v = serde_json::from_str::<serde_json::Value>(l).ok()?;
        (v["event"] == "run_failed").then(|| {
            (
                v["data"]["why"].as_str().unwrap_or("").to_string(),
                v["data"]["detail"].as_str().unwrap_or("").to_string(),
            )
        })
    })
}
