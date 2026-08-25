//! The progress screen: which gate is in flight, and the live lane feed.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::*;

/// Stage index from engine stage strings ("[3/5] ..." → 3).
pub fn stage_no(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('[')?;
    rest.split('/').next()?.parse().ok()
}

pub const PIPELINE: [&str; 6] = ["baseline", "tests", "red", "impl", "green", "review"];

/// One-line pipeline map: done ✓ green · active bold cyan + spinner ·
/// pending dim · failed gate red ✗.
pub fn pipeline_line(cur: Option<usize>, failed: Option<usize>, alive: bool) -> Line<'static> {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, name) in PIPELINE.iter().enumerate() {
        let style = match (Some(i) == failed, cur) {
            (true, _) => Style::new().fg(Color::Red).bold(),
            (false, Some(c)) if i < c => Style::new().fg(Color::Green),
            (false, Some(c)) if i == c && alive => Style::new().fg(Color::Cyan).bold(),
            (false, Some(c)) if i == c => Style::new().fg(Color::Green),
            _ => Style::new().fg(Color::DarkGray),
        };
        let mark = if Some(i) == failed {
            "✗ ".to_string()
        } else if cur.is_some_and(|c| i < c || (i == c && !alive)) {
            "✓ ".to_string()
        } else if cur == Some(i) && alive {
            format!("{} ", spin_frame())
        } else {
            "· ".to_string()
        };
        spans.push(Span::styled(format!("{mark}{name}"), style));
        if i + 1 < PIPELINE.len() {
            spans.push(Span::styled(" ▸ ", Style::new().fg(Color::DarkGray)));
        }
    }
    Line::from(spans)
}

impl App {

    pub fn render_progress(&self, f: &mut Frame, area: Rect) {
        let (log_area, lane_area) = if self.verbose {
            let [a, b] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
            (a, Some(b))
        } else {
            (area, None)
        };
        let mut lines: Vec<Line> = Vec::new();
        let (title, alive) = match &self.job {
            Some(j) => {
                let e = j.started.elapsed().as_secs();
                (
                    format!(
                        " {} · {:02}:{:02} ",
                        j.run_id.as_deref().unwrap_or("planning"),
                        e / 60,
                        e % 60
                    ),
                    j.outcome.is_none(),
                )
            }
            None => (" no job ".into(), false),
        };
        if let Some(j) = &self.job {
            // The gate pipeline strip is a run concept; plan/replan has no gates
            // — its stages show below.
            let is_run = matches!(j.kind, JobKind::Run);
            if is_run {
                // pipeline map: where we are, what's done, what failed
                let mut cur = None;
                let mut failed = None;
                for item in &j.log {
                    match item {
                        LogItem::Stage(s) => {
                            if let Some(n) = stage_no(s) {
                                cur = Some(n);
                            }
                        }
                        LogItem::Gate { gate, ok, .. } => {
                            let idx = match gate.as_str() {
                                "baseline" => Some(0),
                                "red" => Some(2),
                                "green" => Some(4),
                                _ => None,
                            };
                            if *ok {
                                if failed == idx {
                                    failed = None; // rework recovered the gate
                                }
                            } else {
                                failed = idx;
                            }
                        }
                    }
                }
                lines.push(pipeline_line(cur, failed, alive));
                lines.push(Line::raw(""));
            }
            let last_stage = j
                .log
                .iter()
                .rposition(|i| matches!(i, LogItem::Stage(_)));
            for (idx, item) in j.log.iter().enumerate() {
                match item {
                    LogItem::Stage(s) => {
                        let active = alive && Some(idx) == last_stage;
                        // for runs the active gate stage lives in the pipeline strip
                        // above — don't duplicate it; plan stages always show.
                        if is_run && active && stage_no(s).is_some() {
                            continue;
                        }
                        // no strip (plan): spin the active line so it reads as live.
                        let prefix = if active && !is_run {
                            format!(" {} ", spin_frame())
                        } else {
                            "   ".into()
                        };
                        lines.push(Line::raw(format!("{prefix}{s}")));
                    }
                    LogItem::Gate { gate, ok, detail } => {
                        let (mark, color) = if *ok { ("✓", Color::Green) } else { ("✗", Color::Red) };
                        let text = if detail.is_empty() {
                            format!(" {mark} {gate} gate")
                        } else {
                            format!(" {mark} {gate} gate — {detail}")
                        };
                        lines.push(Line::styled(text, Style::new().fg(color)));
                    }
                }
            }
            if !alive {
                lines.push(Line::raw(""));
                lines.push(Line::styled(" finishing ...", Style::new().fg(Color::DarkGray)));
            }
        } else {
            lines.push(Line::raw("  no job running — esc"));
        }
        f.render_widget(
            Paragraph::new(lines).block(boxed(&title, Style::new().bold())),
            log_area,
        );
        if let (Some(area), Some(j)) = (lane_area, &self.job) {
            let h = area.height.saturating_sub(2) as usize;
            let start = j.tail.len().saturating_sub(h);
            let tail: Vec<Line> = j
                .tail
                .iter()
                .skip(start)
                .map(|l| {
                    if l.starts_with("── ") {
                        Line::styled(l.clone(), Style::new().fg(Color::Cyan).bold())
                    } else if let Some(rest) = l.strip_prefix("→ ") {
                        // tool call: name cyan, argument dim
                        let (name, arg) = rest.split_once(' ').unwrap_or((rest, ""));
                        Line::from(vec![
                            Span::styled("→ ", Style::new().fg(Color::DarkGray)),
                            Span::styled(name.to_string(), Style::new().fg(Color::Cyan)),
                            Span::styled(format!(" {arg}"), Style::new().fg(Color::DarkGray)),
                        ])
                    } else if l.starts_with('✗') {
                        Line::styled(l.clone(), Style::new().fg(Color::Red))
                    } else {
                        Line::raw(l.clone())
                    }
                })
                .collect();
            f.render_widget(
                Paragraph::new(tail).block(boxed(
                    &format!(
                        "lane: {} · tools: {} · denials: {}",
                        if j.lane.is_empty() { "—" } else { &j.lane },
                        j.tools,
                        j.denials
                    ),
                    Style::new(),
                )),
                area,
            );
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_stage_index_parses() {
        assert_eq!(stage_no("[0/5] baseline check: node --test"), Some(0));
        assert_eq!(stage_no("[3/5] rework 1/1: implementer gets the failing output"), Some(3));
        assert_eq!(stage_no("no bracket"), None);
    }

}
