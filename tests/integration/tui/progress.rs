use guvnor::tui::progress::stage_no;
use guvnor::tui::{screen_text, App, JobKind};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;

#[test]
fn pipeline_stage_index_parses() {
    assert_eq!(stage_no("[0/5] baseline check: node --test"), Some(0));
    assert_eq!(stage_no("[3/5] rework 1/1: implementer gets the failing output"), Some(3));
    assert_eq!(stage_no("no bracket"), None);
}

/// A line with none of `lane_display`'s own prefixes (`── `, `→ `, `✗`) is the
/// model's own prose, not a tool call — it gets a dim bullet so it lines up
/// with the `→ ` rows around it. A blank line must not grow a stray one.
#[test]
fn assistant_prose_gets_a_dim_bullet_and_a_blank_line_does_not() {
    let mut app = App::for_test();
    app.verbose = true;
    app.start_job(JobKind::Run, Some("id-1".into()), |_tx| Ok(0));
    {
        let job = app.job.as_mut().unwrap();
        job.tail.push_back("→ Read src/a.js".into());
        job.tail.push_back("here is my plan".into());
        job.tail.push_back(String::new());
    }
    let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
    t.draw(|f| app.render_progress(f, Rect::new(0, 0, 80, 20))).unwrap();
    let buf = t.backend().buffer().clone();
    let screen = screen_text(&buf);
    assert!(screen.contains("‣ here is my plan"), "the model's words get a bullet: {screen}");
    assert_eq!(screen.matches('‣').count(), 1, "a blank line must not grow a stray bullet");
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains('‣')).unwrap();
    let x = line.chars().position(|c| c == '‣').unwrap();
    assert_eq!(buf[(x as u16, y as u16)].style().fg, Some(Color::DarkGray), "the bullet is dim");
}
