use guvnor::hookguard::{check_bash, check_read, check_write};

fn no_deny() -> Vec<String> {
    Vec::new()
}

#[test]
fn write_anywhere_inside_repo_ok() {
    // policy: the whole repo is writable, not just test/ or src/
    assert!(check_write("/wt/test/FooSpec.hs", "/wt", &no_deny()).is_ok());
    assert!(check_write("test/FooSpec.hs", "/wt", &no_deny()).is_ok());
    assert!(check_write("/wt/src/Lib.hs", "/wt", &no_deny()).is_ok());
    assert!(check_write("package.json", "/wt", &no_deny()).is_ok()); // root scaffolding
    assert!(check_write("/wt/LICENSE", "/wt", &no_deny()).is_ok());
    assert!(check_write("deep/nested/dir/file.js", "/wt", &no_deny()).is_ok());
}

#[test]
fn write_to_guvnor_control_surfaces_blocked() {
    // a lane rewriting the hook config could disable its own containment
    assert!(check_write("/wt/.claude/settings.json", "/wt", &no_deny()).is_err());
    assert!(check_write(".claude/settings.json", "/wt", &no_deny()).is_err());
    // run evidence a lane can edit is not evidence
    assert!(check_write("/wt/.guvnor/guvnor.toml", "/wt", &no_deny()).is_err());
    assert!(check_write(".guvnor/runs/x/state.json", "/wt", &no_deny()).is_err());
}

#[test]
fn write_to_another_lanes_paths_blocked() {
    // the implementer must not create files tests.patch already owns —
    // otherwise the two patches can't both apply to the verif tree
    let owned = vec!["test/a.test.js".to_string()];
    assert!(check_write("/wt/test/a.test.js", "/wt", &owned).is_err());
    assert!(check_write("test/a.test.js", "/wt", &owned).is_err());
    assert!(check_write("./test/a.test.js", "/wt", &owned).is_err()); // ./ normalized
    // a different file under the same dir is fine (no collision)
    assert!(check_write("test/b.test.js", "/wt", &owned).is_ok());
    assert!(check_write("src/a.js", "/wt", &owned).is_ok());
}

#[test]
fn write_escape_blocked() {
    assert!(check_write("/etc/passwd", "/wt", &no_deny()).is_err());
    assert!(check_write("test/../../src/Evil.hs", "/wt", &no_deny()).is_err());
    assert!(check_write("", "/wt", &no_deny()).is_err());
}

#[test]
fn denied_surfaces_survive_path_spelling() {
    // one denied file, every spelling of it that reaches the same inode
    for p in [
        "/wt//.claude/settings.json",
        "/wt/.//.claude/settings.json",
        ".//.claude/settings.json",
        "/wt///.guvnor/runs/x/state.json",
        ".CLAUDE/settings.json", // case-insensitive filesystem
        ".Guvnor/runs/x/state.json",
        ".claude", // the directory itself: Grep would read the deny file
        ".guvnor",
    ] {
        assert!(check_write(p, "/wt", &no_deny()).is_err(), "write allowed: {p}");
        assert!(check_read(p, "/wt").is_err(), "read allowed: {p}");
    }
}

#[test]
fn reads_are_fenced_to_the_worktree() {
    assert!(check_read("src/a.js", "/wt").is_ok());
    assert!(check_read("/wt/src/a.js", "/wt").is_ok());
    assert!(check_read("", "/wt").is_ok()); // Glob/Grep with no path = cwd
    // the decorrelation hole: the real repo's evidence, by absolute path
    assert!(check_read("/repo/.guvnor/runs/x/tests.patch", "/wt").is_err());
    assert!(check_read(".guvnor/runs/x/tests.patch", "/wt").is_err());
    // the lane's own .claude/deny file names the test files
    assert!(check_read(".claude/settings.json", "/wt").is_err());
    assert!(check_read(".claude/deny", "/wt").is_err());
    // outside the worktree at all — this is what makes the OS prompt
    assert!(check_read("/Users/me/Documents/x", "/wt").is_err());
    assert!(check_read("src/../../secrets", "/wt").is_err());
}

#[test]
fn bash_git_mutations_blocked_others_allowed() {
    assert!(check_bash("git commit -m x").is_err());
    assert!(check_bash("git -C /tmp push origin").is_err());
    assert!(check_bash("cd a && git rebase main").is_err());
    assert!(check_bash("GIT push origin HEAD:main").is_err()); // PATH is case-insensitive
    assert!(check_bash("git status && git diff").is_ok());
    assert!(check_bash("cabal test spec").is_ok());
    // 'commit' outside a git command is fine
    assert!(check_bash("echo commit").is_ok());
}

#[test]
fn bash_cannot_route_around_the_path_guards() {
    // the decorrelation hole: the tests, by any route a shell offers
    assert!(check_bash("cat ../r-tests/test/FooSpec.hs").is_err());
    assert!(check_bash("cat ../../runs/r/tests.patch").is_err());
    assert!(check_bash("grep -r x /repo/.guvnor/runs").is_err());
    assert!(check_bash("rm -rf ../../runs/r").is_err());
    assert!(check_bash("printf x > .claude/settings.json").is_err());
    // and a lane may not hold its own gate
    assert!(check_bash("guvnor approve r --gate work").is_err());
    assert!(check_bash("cd /x && GUVNOR commit r -m x").is_err());
    // ordinary lane work still runs
    assert!(check_bash("node --test").is_ok());
    assert!(check_bash("ls src && cat src/lib.rs").is_ok());
}
