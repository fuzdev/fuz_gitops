//! `repos status --brief`, the `SessionStart` nudge, over a fixture
//! workspace: silent unless the checkout holding the path has something a
//! session should know, one line otherwise, local only, and never a
//! failure its hook would see — only a usage error exits non-zero.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use support::{ClaudeDir, FixtureWorkspace, LiveChild, THIRD_PARTY, proc_start};

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

/// Runs `repos status --brief` in `cwd` with `args` after it, under the
/// hermetic environment plus `env`.
fn brief_with(ws: &FixtureWorkspace, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = ws.command(REPOS, cwd);
    cmd.args(["status", "--brief"]).args(args);
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

/// `brief_with` for a run that must exit `0` with nothing on stderr: its
/// stdout.
fn brief_env(ws: &FixtureWorkspace, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = brief_with(ws, cwd, args, env);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// The brief for `path`, run from the workspace root.
fn brief(ws: &FixtureWorkspace, path: &Path) -> String {
    brief_env(ws, &ws.root(), &[path.to_str().unwrap()], &[])
}

/// `app` with Claude Code's worktrees dir ignored, as a user's global
/// excludes would.
fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    support::write(&app, ".git/info/exclude", ".claude/\n");
    app
}

/// Asserts `line` is `app`'s one line, saying `said` before the fetch age
/// in it.
fn assert_behind(line: &str, said: &str) {
    let rest = line.strip_prefix(&format!("repos: app — {said} (fetched "));
    assert!(
        rest.is_some_and(|rest| rest.ends_with(" ago)\n") && rest.lines().count() == 1),
        "{line}"
    );
}

#[test]
fn silent_on_a_clean_checkout_and_anywhere_no_entry_holds() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.declare_repo("gone", "gone", "");
    // an unregistered clone at the root
    ws.remote("stray", &[]);
    let stray = ws.clone_owned("stray", "stray", &[]);
    ws.commit(&stray, "local");
    ws.write_registry();
    std::fs::create_dir(app.join("src")).unwrap();
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();

    for path in [
        app.clone(),
        app.join("src"),
        ws.root(),
        stray,
        ws.dir("gone"),
        elsewhere.clone(),
        app.join(".git"),
        app.join("nope"),
    ] {
        assert_eq!(brief(&ws, &path), "", "{}", path.display());
    }
    // the path defaults to the cwd, and a relative one is from it
    assert_eq!(brief_env(&ws, &app.join("src"), &[], &[]), "");
    assert_eq!(brief_env(&ws, &ws.root(), &["app/src"], &[]), "");
    // no registry above the path or the cwd
    assert_eq!(brief_env(&ws, &elsewhere, &[], &[]), "");

    // quiet, where plain status says the same entry is dirty
    support::write(&app, "a.txt", "changed\n");
    assert_eq!(brief(&ws, &app), "");
}

#[test]
fn silent_on_every_runtime_failure() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    let registry = ws.write_registry();
    assert_eq!(
        brief(&ws, &app),
        "repos: app — 1 ahead of origin/main (unpushed)\n"
    );

    // an invalid registry: `status` fails with exit 2, `--brief` says nothing
    std::fs::write(&registry, "owners = [\n").unwrap();
    let status = ws
        .command(REPOS, &ws.root())
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(2), "{status:?}");
    assert_eq!(brief(&ws, &app), "");
    std::fs::write(&registry, ws.registry_toml()).unwrap();

    // a `--root` or `--registry` naming nothing
    for flags in [["--root", "nowhere"], ["--registry", "nowhere.toml"]] {
        let out = ws
            .command(REPOS, &ws.root())
            .args(flags)
            .args(["status", "--brief", app.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    }

    // no git on PATH
    let out = brief_with(&ws, &ws.root(), &[app.to_str().unwrap()], &[("PATH", "")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");

    // a failed probe: HEAD's tree deleted, so git's status fails on it
    let tree = ws.git(&app, &["rev-parse", "HEAD:"]);
    let (dir, file) = tree.split_at(2);
    std::fs::remove_file(app.join(".git/objects").join(dir).join(file)).unwrap();
    let json = ws
        .command(REPOS, &ws.root())
        .args(["status", "--json", "app"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(report["entries"][0]["probe_error"].is_string(), "{report}");
    assert_eq!(brief(&ws, &app), "");
}

#[test]
fn usage_errors_exit_2() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    let path = app.to_str().unwrap();
    for args in [
        vec!["--json"],
        vec!["--fetch"],
        vec!["--verbose"],
        vec!["--references"],
        vec![path, path],
    ] {
        let out = brief_with(&ws, &ws.root(), &args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        // no document, even under --json: --brief has none
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.starts_with("error: --brief takes "), "{stderr}");
    }
    // `--timings` goes to stderr, the line unchanged
    let out = brief_with(&ws, &ws.root(), &["--timings", path], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("timings   load "),
        "{out:?}"
    );
}

#[test]
fn says_behind_diverged_and_ahead_with_the_fetch_age() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();

    ws.upstream_commit("app", "main");
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[behind 2]");
    assert_behind(&brief(&ws, &app), "2 behind origin/main");

    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1, behind 2]");
    assert_behind(&brief(&ws, &app), "diverged from origin/main +1 −2");

    // ahead only: pushed nowhere yet; an agent's run says the same
    ws.git(&app, &["reset", "-q", "--hard", "origin/main"]);
    ws.commit(&app, "local-2");
    ws.commit(&app, "local-3");
    ws.assert_track(&app, "main", "[ahead 2]");
    let ahead = "repos: app — 2 ahead of origin/main (unpushed)\n";
    assert_eq!(brief(&ws, &app), ahead);
    assert_eq!(brief_env(&ws, &app, &[], &[("CLAUDECODE", "1")]), ahead);

    // a branch with no upstream, or a detached HEAD, says nothing
    ws.git(&app, &["checkout", "-q", "-b", "loose"]);
    ws.commit(&app, "loose");
    assert_eq!(brief(&ws, &app), "");
    ws.git(&app, &["checkout", "-q", "--detach"]);
    assert_eq!(brief(&ws, &app), "");
}

#[test]
fn says_an_operation_in_progress() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    ws.git(&app, &["checkout", "-q", "-b", "feat"]);
    support::write(&app, "a.txt", "feat\n");
    ws.git(&app, &["commit", "-q", "-am", "feat"]);
    ws.git(&app, &["checkout", "-q", "main"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&app, &["merge", "-q", "feat"]);
    assert!(app.join(".git/MERGE_HEAD").is_file());
    assert_eq!(
        brief(&ws, &app),
        "repos: app — merge in progress; 1 ahead of origin/main (unpushed)\n"
    );
}

#[test]
fn reads_the_checkout_holding_the_path_a_linked_worktree_its_own() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    ws.commit(&app, "local");
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    // outside the workspace: its registry is found through its main checkout
    let wt = ws.outside("app-feat");
    ws.add_worktree(&app, &wt, &["feat"]);
    ws.assert_track(&app, "feat", "[behind 1]");

    assert_eq!(
        brief(&ws, &app),
        "repos: app — 1 ahead of origin/main (unpushed)\n"
    );
    assert_behind(&brief(&ws, &wt), "1 behind origin/feat");
    assert_behind(&brief_env(&ws, &wt, &[], &[]), "1 behind origin/feat");
    // the registry is found from the path, whatever the cwd
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    assert_eq!(
        brief_env(&ws, &elsewhere, &[app.to_str().unwrap()], &[]),
        "repos: app — 1 ahead of origin/main (unpushed)\n"
    );

    // a session in the primary is the primary's alone, recorded under the
    // fixture's `$HOME/.claude`
    let home_claude = ClaudeDir::new(ws.base().join("home/.claude"));
    let other = LiveChild::spawn_in(&app);
    home_claude.session(other.pid(), &other.proc_start(), &app);
    assert_eq!(
        brief(&ws, &app),
        "repos: app — another live session is working in this checkout; 1 ahead of \
         origin/main (unpushed)\n"
    );
    assert_behind(&brief(&ws, &wt), "1 behind origin/feat");
}

#[test]
fn speaks_for_the_workspace_a_registry_kept_in_a_repo_is_linked_at() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let meta = ws.owned_repo("meta", &[]);
    support::write(&meta, ".git/info/exclude", ".claude/\n");
    // committed in `meta`, linked at the root
    ws.write_registry_in(&meta);
    ws.git(&meta, &["add", "repos.toml"]);
    ws.git(&meta, &["commit", "-q", "-m", "registry"]);
    ws.assert_track(&meta, "main", "[ahead 1]");
    let wt = meta.join(".claude/worktrees/agent");
    ws.add_worktree(&meta, &wt, &["-b", "agent"]);
    ws.assert_clean(&meta);
    assert!(wt.join("repos.toml").is_file());
    std::fs::create_dir(wt.join("src")).unwrap();

    // found walking up from the path: `meta`'s own registry first, then its
    // link at the root, which roots the workspace
    let said = "repos: meta — 1 ahead of origin/main (unpushed)\n";
    assert_eq!(brief(&ws, &meta), said);
    assert_eq!(brief_env(&ws, &meta, &[], &[]), said);
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    assert_eq!(
        brief_env(&ws, &elsewhere, &[meta.to_str().unwrap()], &[]),
        said
    );
    // a worktree's own copy is passed over for the main checkout's
    assert_eq!(brief_env(&ws, &elsewhere, &[wt.to_str().unwrap()], &[]), "");
    ws.git(&wt, &["branch", "-q", "--set-upstream-to", "origin/main"]);
    ws.commit(&wt, "wip");
    ws.assert_track(&wt, "agent", "[ahead 2]");
    let said_wt = "repos: meta — 2 ahead of origin/main (unpushed)\n";
    assert_eq!(brief_env(&ws, &wt.join("src"), &[], &[]), said_wt);

    // the path is walked as the kernel resolves it: through a symlink into
    // the worktree (lexically, no checkout and no registry entry above it)
    let alias = ws.outside("alias");
    std::os::unix::fs::symlink(wt.join("src"), &alias).unwrap();
    assert_eq!(brief(&ws, &alias), said_wt);
    // and `..` after one: `decoy/..` is the workspace root, not `elsewhere`
    // (its stray link to the registry passed over)
    std::os::unix::fs::symlink(meta.join("repos.toml"), elsewhere.join("repos.toml")).unwrap();
    std::os::unix::fs::symlink(ws.dir("app"), elsewhere.join("decoy")).unwrap();
    let dotted = elsewhere.join("decoy/../meta");
    assert_eq!(dotted.canonicalize().unwrap(), meta);
    assert_eq!(brief(&ws, &dotted), said);
    // nothing there: silent
    assert_eq!(brief(&ws, &elsewhere.join("decoy/../nope")), "");
}

#[test]
fn silent_on_a_root_refused_in_an_entrys_checkout() {
    let mut ws = FixtureWorkspace::new();
    let meta = ws.owned_repo("meta", &[]);
    // committed in `meta` and never linked at the root
    ws.write_registry_in(&meta);
    std::fs::remove_file(ws.root().join("repos.toml")).unwrap();
    ws.git(&meta, &["add", "repos.toml"]);
    ws.git(&meta, &["commit", "-q", "-m", "registry"]);
    ws.assert_track(&meta, "main", "[ahead 1]");
    let status = ws.command(REPOS, &meta).arg("status").output().unwrap();
    assert_eq!(status.status.code(), Some(2), "{status:?}");

    assert_eq!(brief(&ws, &meta), "");
    assert_eq!(brief_env(&ws, &meta, &[], &[]), "");
    // linked, it speaks
    std::os::unix::fs::symlink(meta.join("repos.toml"), ws.root().join("repos.toml")).unwrap();
    assert_eq!(
        brief(&ws, &meta),
        "repos: meta — 1 ahead of origin/main (unpushed)\n"
    );
}

#[test]
fn an_agent_worktree_hears_only_of_sessions_working_in_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    // where Claude Code puts a `--worktree` session's, or a subagent's
    let wt1 = app.join(".claude/worktrees/wt1");
    ws.add_worktree(&app, &wt1, &["-b", "wt1"]);
    let claude = ClaudeDir::new(ws.outside("claude"));
    // the caller in the worktree, another session in the primary
    let me = std::process::id();
    claude.session(me, &proc_start(me), &wt1);
    let other = LiveChild::spawn_in(&app);
    claude.session(other.pid(), &other.proc_start(), &app);
    let me_s = me.to_string();
    let env = [
        ("CLAUDE_CONFIG_DIR", claude.0.to_str().unwrap()),
        ("CLAUDE_PID", me_s.as_str()),
    ];
    let other_here = "repos: app — another live session is working in this checkout\n";

    // the session in the primary holds the worktree for its subagents, but
    // doesn't work in it: the worktree's own session isn't told of it
    assert_eq!(brief_env(&ws, &wt1, &[], &env), "");
    assert_eq!(brief_env(&ws, &app, &[], &env), other_here);
    let status = ws
        .command(REPOS, &ws.root())
        .args(["status", "--json", "app"])
        .envs(env)
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let wt1_s = wt1.to_str().unwrap();
    let busy_in_wt1 = report["entries"][0]["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == wt1_s)
        .map(|c| c["busy"].clone());
    assert_eq!(
        busy_in_wt1,
        Some(serde_json::json!([
            {"pid": other.pid(), "cwd": app.to_str().unwrap(), "source": "session_file"}
        ]))
    );

    // locked for it as Claude Code locks a subagent's worktree: it works there
    let reason = format!(
        "claude agent wt1 (pid {} start {})",
        other.pid(),
        other.proc_start()
    );
    ws.git(&app, &["worktree", "lock", "--reason", &reason, wt1_s]);
    assert_eq!(brief_env(&ws, &wt1, &[], &env), other_here);
}

#[test]
fn a_reference_or_a_pin_says_only_sessions_and_operations() {
    let mut ws = FixtureWorkspace::new();
    for name in ["lib", "fork"] {
        ws.remote(name, &[]);
    }
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    ws.declare_reference("fork", support::OWNER, "fork", "pinned = true");
    ws.write_registry();
    let lib = ws.clone_third_party("lib", "lib", &[]);
    let fork = ws.clone_owned("fork", "fork", &[]);
    // the reference with local work on a branch ahead of origin's
    ws.commit(&lib, "local");
    ws.assert_track(&lib, "main", "[ahead 1]");
    // the pin on a branch behind a moved origin
    ws.upstream_commit("fork", "main");
    ws.git(&fork, &["fetch", "-q", "origin"]);
    ws.assert_track(&fork, "main", "[behind 1]");
    for repo in [&lib, &fork] {
        assert_eq!(brief(&ws, repo), "", "{}", repo.display());
    }

    let claude = ClaudeDir::new(ws.outside("claude"));
    let other = LiveChild::spawn_in(&lib);
    claude.session(other.pid(), &other.proc_start(), &lib);
    let env = [("CLAUDE_CONFIG_DIR", claude.0.to_str().unwrap())];
    assert_eq!(
        brief_env(&ws, &lib, &[], &env),
        "repos: lib — another live session is working in this checkout\n"
    );
    assert_eq!(brief_env(&ws, &fork, &[], &env), "");
}

#[test]
fn says_the_other_sessions_never_the_caller() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    ws.commit(&app, "local");
    let claude = ClaudeDir::new(ws.outside("claude"));
    // the caller: this test process, the binary's parent, as a hook's
    // session is its ancestor, with its session file already written
    let me = std::process::id();
    claude.session(me, &proc_start(me), &app);
    let me_s = me.to_string();
    let as_caller = [
        ("CLAUDE_CONFIG_DIR", claude.0.to_str().unwrap()),
        ("CLAUDE_PID", me_s.as_str()),
        ("CLAUDECODE", "1"),
    ];
    let ahead = "1 ahead of origin/main (unpushed)";
    assert_eq!(
        brief_env(&ws, &app, &[], &as_caller),
        format!("repos: app — {ahead}\n")
    );
    // without `CLAUDE_PID` the caller is just another session
    assert_eq!(
        brief_env(&ws, &app, &[], &as_caller[..1]),
        format!("repos: app — another live session is working in this checkout; {ahead}\n")
    );

    // another session in the checkout, and one elsewhere that isn't in it
    let other = LiveChild::spawn_in(&app);
    claude.session(other.pid(), &other.proc_start(), &app.join("src"));
    let at_root = LiveChild::spawn_in(&ws.root());
    claude.session(at_root.pid(), &at_root.proc_start(), &ws.root());
    assert_eq!(
        brief_env(&ws, &app, &[], &as_caller),
        format!("repos: app — another live session is working in this checkout; {ahead}\n")
    );
    let second = LiveChild::spawn_in(&app);
    claude.session(second.pid(), &second.proc_start(), &app);
    assert_eq!(
        brief_env(&ws, &app, &[], &as_caller),
        format!("repos: app — 2 other live sessions are working in this checkout; {ahead}\n")
    );

    // busy detection unavailable: nothing said of sessions, the rest still
    claude.write_raw(&format!("{}.json", other.pid()), "{not json");
    assert_eq!(
        brief_env(&ws, &app, &[], &as_caller),
        format!("repos: app — {ahead}\n")
    );
}

#[test]
fn writes_nothing_and_reaches_no_remote() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.write_registry();
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    // stat info the index no longer matches, which a refreshing status
    // would write back
    support::set_mtime(
        &app.join("a.txt"),
        SystemTime::now() - Duration::from_secs(60),
    );
    let ssh_calls = ws.ssh_log().len();
    let before = support::snapshot_git_dir(&app.join(".git"));
    assert_behind(&brief(&ws, &app), "1 behind origin/main");
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&app.join(".git")));
    assert_eq!(ws.ssh_log().len(), ssh_calls);
}
