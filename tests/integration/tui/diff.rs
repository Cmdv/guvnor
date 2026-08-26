use guvnor::tui::{line_text, patch_sections, press, render_diff, screen_text, DiffView, Section};
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::text::Line;

const PATCH: &str = "\
diff --git a/src/a.js b/src/a.js
index d10a038..5886630 100644
--- a/src/a.js
+++ b/src/a.js
@@ -1,2 +1,2 @@
-old line
+new line
 context
diff --git a/test/b.test.js b/test/b.test.js
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/test/b.test.js
@@ -0,0 +1,2 @@
+one
+two
";

/// The wrapped body is cached per width, so a resize has to invalidate it or
/// the rows drawn and the cursor's idea of them drift apart.
#[test]
fn the_wrap_cache_follows_the_width() {
    let long = "x ".repeat(60);
    let s = Section::new(Line::raw("head"), vec![Line::raw(long)]);
    let wide = s.wrapped(120).len();
    let narrow = s.wrapped(40).len();
    assert!(narrow > wide, "narrower must wrap to more rows: {narrow} vs {wide}");
    // and coming back gives the same answer, not a stale one
    assert_eq!(s.wrapped(120).len(), wide);
    assert_eq!(s.open_rows(40) as usize, narrow + 1, "plus the trailing blank");
}

/// One row per file, the plumbing gone, and the counts taken from the hunks
/// rather than from anything the model said.
#[test]
fn a_patch_becomes_one_row_per_file() {
    let s = patch_sections(PATCH);
    assert_eq!(s.len(), 2, "one section per file");
    assert_eq!(line_text(&s[0].head), "modified src/a.js  +1 -1");
    assert_eq!(line_text(&s[1].head), "new      test/b.test.js  +2 -0");
    let body: String = s.iter().flat_map(|f| f.body.iter()).map(line_text).collect();
    for noise in ["index ", "--- ", "+++ ", "diff --git"] {
        assert!(!body.contains(noise), "{noise:?} survived into the body: {body:?}");
    }
    assert!(body.contains("@@ -1,2 +1,2 @@"), "hunk headers stay: {body:?}");
    assert!(body.contains("+new line") && body.contains("-old line"));
    assert!(s.iter().all(|f| !f.open), "everything starts collapsed");
}

/// Verbatim `git diff --cached` output for deleting two `--` comments from a
/// Haskell file (a shipped language preset). The `-` prefix makes each one
/// `--- ...`, which is file-header syntax outside a hunk and the human's own
/// deleted code inside one. Eat them and the work gate is judged on a diff
/// that is missing lines, with a `-N` count that does not match.
#[test]
fn removed_comment_lines_are_not_mistaken_for_headers() {
    let patch = "\
diff --git a/Mean.hs b/Mean.hs
index 31d3074..efb9aa1 100644
--- a/Mean.hs
+++ b/Mean.hs
@@ -1,4 +1,2 @@
--- | Compute the mean
--- second comment
 mean :: [Double] -> Double
 mean xs = sum xs / n
";
    let s = patch_sections(patch);
    assert_eq!(s.len(), 1);
    assert_eq!(line_text(&s[0].head), "modified Mean.hs  +0 -2");
    let body: String = s[0].body.iter().map(line_text).collect();
    assert!(body.contains("-- | Compute the mean"), "removed line vanished: {body:?}");
    assert!(body.contains("-- second comment"), "removed line vanished: {body:?}");
}

/// Collapsed means collapsed: the tab opens as a list of what changed, and
/// the hunks only reach the screen for the row you opened.
#[test]
fn the_list_draws_closed_and_opens_one_row_at_a_time() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut v = DiffView { sections: patch_sections(PATCH), ..Default::default() };
    let draw = |v: &DiffView| -> String {
        let mut t = Terminal::new(TestBackend::new(60, 12)).unwrap();
        t.draw(|f| render_diff(f, Rect::new(0, 0, 60, 12), v)).unwrap();
        screen_text(t.backend().buffer())
    };
    let closed = draw(&v);
    assert!(closed.contains("▸ modified src/a.js"), "{closed}");
    assert!(closed.contains("▸ new      test/b.test.js"), "{closed}");
    assert!(!closed.contains("new line"), "no hunks until you ask: {closed}");
    v.handle(&press(KeyCode::Char(' ')));
    let open = draw(&v);
    assert!(open.contains("▾ modified src/a.js"), "the marker turns: {open}");
    assert!(open.contains("+new line"), "and the hunk is there: {open}");
    assert!(open.contains("▸ new      test/b.test.js"), "the other stays shut: {open}");
}

/// The cursor walks the rows, space opens one, and an open row pushes the
/// rows under it down — which is what `head_y` has to know for the scroll.
#[test]
fn space_opens_a_row_and_moves_the_ones_below_it() {
    let mut v = DiffView {
        sections: patch_sections(PATCH),
        ..Default::default()
    };
    // Built directly (no render_diff pass), so body_w defaults to 0; set
    // it wide enough that this fixture's body lines don't actually wrap,
    // so head_y still counts one row per body line below.
    v.body_w.set(200);
    assert_eq!(v.head_y(0), 1);
    assert_eq!(v.head_y(1), 2, "collapsed rows are one line each");
    assert!(v.handle(&press(KeyCode::Char(' '))));
    assert!(v.sections[0].open, "space opens the row under the cursor");
    // 4 body lines (@@, -, +, context) + a trailing blank
    assert_eq!(v.head_y(1), 7, "the row below is pushed past the body");
    assert!(v.handle(&press(KeyCode::Down)));
    assert_eq!(v.sel, 1);
    assert!(v.handle(&press(KeyCode::Down)));
    assert_eq!(v.sel, 1, "the cursor stops at the last row");
    // ↵ belongs to the gate, not to us
    assert!(!v.handle(&press(KeyCode::Enter)));
}
