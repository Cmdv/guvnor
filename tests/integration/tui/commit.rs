use guvnor::state::Status;
use guvnor::tui::{
    commit_key, commit_now, lines_text, press, App, CommitView, Go, Screen, StageView,
    SUBJECT_MAX,
};
use ratatui::crossterm::event::KeyCode;

#[test]
fn a_message_splits_the_way_git_reads_it() {
    let mut v = CommitView::new("x".into());
    v.set_message("add rolling stats\n\nMean and median over a window,\nguarded against empty input.");
    let (s, b) = v.parts();
    assert_eq!(s, "add rolling stats");
    assert!(b.starts_with("Mean and median"), "{b:?}");
    // subject-only drops the body but keeps it in the box, so the toggle
    // is reversible — deleting the text would not be
    v.with_body = false;
    let (s2, b2) = v.parts();
    assert_eq!(s2, s);
    assert!(b2.is_empty());
    v.with_body = true;
    assert_eq!(v.parts().1, b, "the body must survive the round trip");
    // a one-liner has no body to find
    v.set_message("fix the thing");
    assert_eq!(v.parts(), ("fix the thing".into(), String::new()));
}

#[test]
fn tab_reaches_every_box_and_comes_back() {
    let mut app = App::for_test();
    app.commit = Some(CommitView::new("x".into()));
    let focus = |a: &App| a.commit.as_ref().unwrap().focus;
    let start = focus(&app);
    for _ in 0..2 {
        commit_key(&mut app, &press(KeyCode::Tab));
    }
    assert!(focus(&app) == start, "two boxes, so two tabs come back round");
    // and backwards
    commit_key(&mut app, &press(KeyCode::Tab));
    commit_key(&mut app, &press(KeyCode::BackTab));
    assert!(focus(&app) == start);
    // esc closes the modal but keeps the draft: a message you spent a minute
    // on, or paid a lane to write, must survive stepping out to re-read a diff
    app.commit.as_mut().unwrap().set_message("add rolling stats\n\nwhy");
    commit_key(&mut app, &press(KeyCode::Esc));
    assert!(!app.commit_open(), "esc must close it");
    assert!(app.commit.is_some(), "esc must not delete the draft");
    app.open_commit("x");
    assert!(app.commit_open());
    assert_eq!(app.commit.as_ref().unwrap().parts().0, "add rolling stats");
    // a different run is a different change: never show it x's words
    app.open_commit("y");
    assert!(!matches!(&app.commit, Some(v) if v.id == "x"));
}

#[test]
fn commit_refuses_an_empty_or_overlong_subject() {
    let mut app = App::for_test();
    app.commit = Some(CommitView::new("x".into()));
    // nothing typed: it must say so rather than write an empty commit
    assert!(commit_now(&mut app).is_none());
    assert!(app.toast.as_ref().unwrap().0.contains("subject"));
    // over the limit: refused with the count, not silently truncated —
    // truncating someone's commit message is worse than refusing it
    app.commit.as_mut().unwrap().set_message(&"x".repeat(SUBJECT_MAX + 1));
    app.toast = None;
    assert!(commit_now(&mut app).is_none());
    let msg = &app.toast.as_ref().unwrap().0;
    assert!(msg.contains(&(SUBJECT_MAX + 1).to_string()), "{msg}");
    // the run is untouched either way: nothing reached the repo
    assert!(app.commit.is_some());
    assert!(app.job.is_none(), "a refused subject must not reach the repo at all");
}

/// Staging spawns a handful of git processes, `git status` among them. Run on
/// the UI thread that is a frozen screen with no frame drawn until it is
/// over, so the three landing verbs go through a job like every other engine
/// call. They keep the screen rather than taking it: each is brief and
/// reports with a toast.
#[test]
fn the_landing_verbs_do_not_run_on_the_ui_thread() {
    for go in [Go::Stage("x".into()), Go::Unstage("x".into())] {
        let mut app = App::for_test();
        app.apply(go);
        assert!(app.job.is_some(), "the verb must be handed to a job");
        assert!(matches!(app.screen, Screen::Runs), "and must not take the screen");
        assert!(app.toast.is_some(), "with something on screen saying so");
    }
    // commit too, once it has a subject worth writing
    let mut app = App::for_test();
    let mut v = CommitView::new("x".into());
    v.set_message("add a thing");
    app.commit = Some(v);
    assert!(commit_now(&mut app).is_none());
    assert!(app.job.is_some(), "commit must be handed to a job as well");
}

#[test]
fn the_message_modal_arms_copy_not_commit() {
    let v = CommitView::new("x".into());
    // a stray ↵ must never write history or fire the paid draft lane, so the
    // armed default is the harmless one — commit is stepped over to on purpose
    assert_eq!(v.buttons.labels, ["generate", "copy", "commit"]);
    assert_eq!(v.buttons.labels[v.buttons.sel], "copy");
}

/// The stage box offers exactly the moves the tree is in a state for, and
/// nothing else — a button for something impossible is worse than no button.
#[test]
fn the_stage_box_offers_only_what_the_tree_allows() {
    let labels = |st: Status| StageView::build("nope", &st, None).buttons.labels.to_vec();
    assert_eq!(labels(Status::Reviewed), ["stage"], "not in the tree yet");
    assert_eq!(labels(Status::Staged), ["commit", "unstage"], "in the tree: keep it or not");
    assert!(labels(Status::Committed).is_empty(), "committed: nothing left to do");
    // ...and committed builds an empty row: `handle` answers, but there is
    // no label at any index and the dispatch guards on the same state
    let mut none = StageView::build("nope", &Status::Committed, None).buttons;
    assert_eq!(none.labels.get(none.handle(KeyCode::Enter).unwrap_or(0)), None);
}

/// The words under the file list are the whole explanation of why landing is
/// two steps, so each state has to say something different and true.
#[test]
fn the_stage_box_explains_itself_in_each_state() {
    let text = |v: &StageView| lines_text(&v.explain());
    let words = |st: Status| text(&StageView::build("nope", &st, None));
    assert!(words(Status::Reviewed).contains("Staging writes these files"));
    assert!(words(Status::Reviewed).contains("Nothing is committed until you ask"));
    assert!(words(Status::Staged).contains("git diff --cached"));
    assert!(words(Status::Staged).contains("unstage"));
    assert!(words(Status::Committed).contains("does not push"));

    // an unstaged edit is named, since `git commit` will silently leave it out
    let mut v = StageView::build("nope", &Status::Staged, None);
    v.edited = vec!["src/a.js".into()];
    assert!(text(&v).contains("NOT in the commit") && text(&v).contains("src/a.js"));
}

#[test]
fn landed_runs_are_named_in_the_past_tense() {
    // the run list and the tab strip read this: a staged run is not a
    // committed one, and neither is a failure
    assert_eq!(Status::Staged.to_string(), "staged");
    assert_eq!(Status::Committed.to_string(), "committed");
}
