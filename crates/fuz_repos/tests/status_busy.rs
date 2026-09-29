//! Busy detection: the sessions reader over a fixture Claude Code config
//! dir whose files name real live pids (the test's own children), the
//! scoping of live sessions to checkouts, and the holds they put on sync's
//! actions.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fuz_repos::busy::Sessions;
use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::EntryStatus;
use fuz_repos::sessions::{
    LiveSessions, Session, SessionSource, SessionsSource, Unavailable, read_live_sessions,
};
use fuz_repos::state::{
    CleanupReason, HeldBy, Prune, SyncAction, UnprobedHead, UnprobedWhy, Verdict,
};
use fuz_repos::status::StatusRun;
use support::{
    ClaudeDir, FixtureWorkspace, LiveChild, branch, dead_pid, find_entry, own_pid_domain,
    proc_start, roster_worker,
};

const fn push(commits: u32) -> SyncAction {
    SyncAction::Push { commits }
}

const fn ff(commits: u32) -> SyncAction {
    SyncAction::FastForward { commits }
}

const fn held(action: SyncAction, by: HeldBy) -> Verdict {
    Verdict::Held { action, by }
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}

/// A session no process backs, for the scoping alone: its start time is
/// never checked.
fn session(pid: u32, cwd: &Path, source: SessionSource) -> Session {
    Session::at(pid, 0, path(cwd), source)
}

/// A session of one of the test's own children, with its start time, as
/// the reader vouches for it.
fn live_session(child: &LiveChild, cwd: &Path, source: SessionSource) -> Session {
    Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        path(cwd),
        source,
    )
}

/// `live_session` for a child spawned where the test runs
/// (`LiveChild::spawn`): its process's cwd.
fn child_session(child: &LiveChild, cwd: &Path, source: SessionSource) -> Session {
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_ne!(here, cwd);
    Session {
        process_cwd: Some(path(&here)),
        ..live_session(child, cwd, source)
    }
}

/// Where the tool would look given `dirs`, with no `CLAUDE_PID`.
fn source(dirs: &[&ClaudeDir]) -> SessionsSource {
    SessionsSource {
        config_dirs: Ok(dirs.iter().map(|d| d.0.clone()).collect()),
        claude_pid: None,
        ancestors: BTreeMap::new(),
    }
}

/// Reads `claude` as the tool would, no caller excluded.
fn read(claude: &ClaudeDir) -> LiveSessions {
    read_live_sessions(&source(&[claude]))
}

/// Reads `claude` with `CLAUDE_PID` set to `caller`'s pid, and `caller`
/// among this process's ancestors when `ancestor`.
fn read_as(claude: &ClaudeDir, caller: &LiveChild, ancestor: bool) -> LiveSessions {
    let mut source = source(&[claude]);
    source.claude_pid = Some(caller.pid());
    if ancestor {
        let start = caller.proc_start().parse().unwrap();
        source.ancestors.insert(caller.pid(), start);
    }
    read_live_sessions(&source)
}

/// A fresh config dir under the fixture's tempdir.
fn claude_dir(ws: &FixtureWorkspace, name: &str) -> ClaudeDir {
    ClaudeDir::new(ws.outside(name))
}

/// A session file for `pid` as Claude Code writes it.
fn session_doc(pid: u32, proc_start: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "pid": pid, "procStart": proc_start, "cwd": path(cwd), "pidDomain": own_pid_domain(),
    })
}

/// Sorted as the reader lists sessions: by pid, then cwd.
fn by_pid(mut sessions: Vec<Session>) -> LiveSessions {
    sessions.sort_by(|a, b| (a.pid, &a.cwd).cmp(&(b.pid, &b.cwd)));
    LiveSessions::Known(sessions)
}

#[test]
fn only_live_sessions_count() {
    let ws = FixtureWorkspace::new();
    let claude = claude_dir(&ws, "claude");
    let cwd = ws.root();
    let live = LiveChild::spawn();
    claude.session(live.pid(), &live.proc_start(), &cwd);
    // exited: the pid is free
    let dead = dead_pid();
    claude.session(dead, "12345", &cwd);
    // a live pid, but not the process that started then: the pid was reused
    let reused = LiveChild::spawn();
    let other_start = (reused.proc_start().parse::<u64>().unwrap() + 1).to_string();
    claude.session(reused.pid(), &other_start, &cwd);
    // garbage whose pid no process has: nothing live can be behind it
    let dead_garbage = dead_pid();
    claude.write_raw(&format!("{dead_garbage}.json"), "{not json");
    // not `<pid>.json`: never opened, whatever they hold
    claude.write_raw(&format!("{}.0123abcd.key", reused.pid()), "{not json");
    claude.write_raw(&format!("{}.json.tmp", reused.pid()), "{not json");
    claude.write_raw("notes.json", "{not json");
    std::fs::create_dir(claude.0.join("sessions/sub")).unwrap();

    assert_eq!(
        read(&claude),
        LiveSessions::Known(vec![child_session(&live, &cwd, SessionSource::SessionFile)])
    );
}

#[test]
fn no_sessions_dir_is_nothing_live() {
    let ws = FixtureWorkspace::new();
    let claude = claude_dir(&ws, "claude");
    assert!(!claude.0.join("sessions").exists());
    assert_eq!(read(&claude), LiveSessions::Known(vec![]));
    // an empty one too
    std::fs::create_dir(claude.0.join("sessions")).unwrap();
    assert_eq!(read(&claude), LiveSessions::Known(vec![]));
    // and a config dir that isn't there
    let missing = ClaudeDir(ws.outside("missing"));
    assert_eq!(
        read_live_sessions(&source(&[&missing, &claude])),
        LiveSessions::Known(vec![])
    );
    // not knowing where to look is another matter
    let mut unknown = source(&[&claude]);
    unknown.config_dirs = Err(Unavailable::HomeUnknown);
    assert_eq!(
        read_live_sessions(&unknown),
        LiveSessions::Unavailable(Unavailable::HomeUnknown)
    );
}

#[test]
fn every_config_dir_is_read() {
    let ws = FixtureWorkspace::new();
    let a = claude_dir(&ws, "a");
    let b = claude_dir(&ws, "b");
    let in_a = LiveChild::spawn();
    let in_b = LiveChild::spawn();
    let worker_in_b = LiveChild::spawn();
    a.session(in_a.pid(), &in_a.proc_start(), &ws.root());
    b.session(in_b.pid(), &in_b.proc_start(), &ws.root());
    let app = ws.dir("app");
    b.roster(&serde_json::json!({"workers": {
        "w": roster_worker(worker_in_b.pid(), &worker_in_b.proc_start(), &app, (1, "1")),
    }}));
    assert_eq!(
        read_live_sessions(&source(&[&a, &b])),
        by_pid(vec![
            child_session(&in_a, &ws.root(), SessionSource::SessionFile),
            child_session(&in_b, &ws.root(), SessionSource::SessionFile),
            child_session(&worker_in_b, &app, SessionSource::RosterWorker),
        ])
    );
    // one it can't vouch for, in either, fails the whole read
    let file = a.write_raw(&format!("{}.json", in_a.pid()), "{not json");
    let got = read_live_sessions(&source(&[&b, &a]));
    assert!(
        matches!(&got, LiveSessions::Unavailable(Unavailable::Unparseable { path, .. })
            if *path == self::path(&file)),
        "{got:?}"
    );
}

#[test]
fn a_session_file_wins_over_a_roster_worker_in_another_config_dir() {
    let ws = FixtureWorkspace::new();
    let a = claude_dir(&ws, "a");
    let b = claude_dir(&ws, "b");
    let app = ws.dir("app");
    let live = LiveChild::spawn();
    // the worker in the dir read first, its session file in the other
    a.roster(&serde_json::json!({"workers": {
        "w": roster_worker(live.pid(), &live.proc_start(), &app, (1, "1")),
    }}));
    b.session(live.pid(), &live.proc_start(), &app);
    let once = LiveSessions::Known(vec![child_session(&live, &app, SessionSource::SessionFile)]);
    assert_eq!(read_live_sessions(&source(&[&a, &b])), once);
    assert_eq!(read_live_sessions(&source(&[&b, &a])), once);
}

#[test]
fn a_relative_config_dir_makes_detection_unavailable() {
    let ws = FixtureWorkspace::new();
    let claude = claude_dir(&ws, "claude");
    let mut source = source(&[&claude]);
    source
        .config_dirs
        .as_mut()
        .unwrap()
        .push(PathBuf::from("claude"));
    assert_eq!(
        read_live_sessions(&source),
        LiveSessions::Unavailable(Unavailable::RelativeConfigDir {
            path: "claude".into()
        })
    );
}

#[test]
fn a_session_it_cannot_vouch_for_makes_detection_unavailable() {
    let ws = FixtureWorkspace::new();
    let cwd = ws.root();
    let live = LiveChild::spawn();
    let pid = live.pid();
    let start = live.proc_start();
    let unparseable = |claude: &ClaudeDir, file: &Path| match read(claude) {
        LiveSessions::Unavailable(Unavailable::Unparseable { path, .. }) => {
            assert_eq!(path, self::path(file));
        }
        other => panic!("{other:?}"),
    };
    let unreadable = |claude: &ClaudeDir, file: &Path| match read(claude) {
        LiveSessions::Unavailable(Unavailable::Unreadable { path, .. }) => {
            assert_eq!(path, self::path(file));
        }
        other => panic!("{other:?}"),
    };

    // garbage for a live pid
    let claude = claude_dir(&ws, "garbage");
    let file = claude.write_raw(&format!("{pid}.json"), "{not json");
    unparseable(&claude, &file);

    // a field the reader needs gone: a format change
    let claude = claude_dir(&ws, "no-domain");
    let doc = serde_json::json!({"pid": pid, "procStart": start, "cwd": path(&cwd)});
    let file = claude.write_raw(&format!("{pid}.json"), &doc.to_string());
    unparseable(&claude, &file);

    // a procStart that isn't one
    let claude = claude_dir(&ws, "bad-start");
    let file = claude.session(pid, "soon", &cwd);
    unparseable(&claude, &file);

    // a cwd that says nothing of where it works
    let claude = claude_dir(&ws, "relative-cwd");
    let file = claude.session(pid, &start, Path::new("app"));
    unparseable(&claude, &file);

    // a pid other than its name's
    let claude = claude_dir(&ws, "renamed");
    let other = LiveChild::spawn();
    let doc = session_doc(other.pid(), &other.proc_start(), &cwd);
    let file = claude.write_raw(&format!("{pid}.json"), &doc.to_string());
    unparseable(&claude, &file);

    // not a file: a dir by a session file's name
    let claude = claude_dir(&ws, "dir-by-name");
    let file = claude.0.join(format!("sessions/{pid}.json"));
    std::fs::create_dir_all(&file).unwrap();
    unreadable(&claude, &file);

    // over the size cap, however well-formed
    let claude = claude_dir(&ws, "oversized");
    let doc = session_doc(pid, &start, &cwd).to_string();
    let file = claude.write_raw(
        &format!("{pid}.json"),
        &format!("{doc}{}", " ".repeat(4 * 1024 * 1024)),
    );
    unreadable(&claude, &file);

    // another pid namespace: this /proc can't speak for its pid, live here
    // or not
    let foreign = "linux:0123456789abcdef0123456789abcdef:pid:[4026532999]";
    assert_ne!(foreign, own_pid_domain());
    for (name, pid) in [("foreign-live", pid), ("foreign-dead", dead_pid())] {
        let claude = claude_dir(&ws, name);
        let file = claude.session_in(pid, &start, &cwd, foreign);
        assert_eq!(
            read(&claude),
            LiveSessions::Unavailable(Unavailable::ForeignPidDomain {
                path: path(&file),
                pid_domain: foreign.into(),
                source: SessionSource::SessionFile,
            }),
            "{name}"
        );
    }

    // a sessions path that can't be listed
    let claude = claude_dir(&ws, "not-a-dir");
    std::fs::write(claude.0.join("sessions"), "").unwrap();
    unreadable(&claude, &claude.0.join("sessions"));

    // a roster that isn't a file
    let claude = claude_dir(&ws, "roster-dir");
    let roster = claude.0.join("daemon/roster.json");
    std::fs::create_dir_all(&roster).unwrap();
    unreadable(&claude, &roster);
}

#[test]
fn roster_workers_join_the_sessions_by_pid_and_cwd() {
    let ws = FixtureWorkspace::new();
    let claude = claude_dir(&ws, "claude");
    let root = ws.root();
    let app = ws.dir("app");
    let same = LiveChild::spawn();
    claude.session(same.pid(), &same.proc_start(), &app);
    let moved = LiveChild::spawn();
    claude.session(moved.pid(), &moved.proc_start(), &root);
    let worker = LiveChild::spawn();
    let caller = LiveChild::spawn();
    let callers_worker = LiveChild::spawn();
    let dead = dead_pid();
    let callers = (caller.pid(), caller.proc_start());
    claude.roster(&serde_json::json!({
        "proto": 1,
        "supervisorPid": 1,
        "workers": {
            // in a session file too, at its cwd: counted once, as the
            // session file's
            "a": roster_worker(same.pid(), &same.proc_start(), &app, (1, "1")),
            // in a session file at another cwd: both held
            "b": roster_worker(moved.pid(), &moved.proc_start(), &app, (1, "1")),
            "c": roster_worker(worker.pid(), &worker.proc_start(), &app, (2, "1")),
            // exited: the roster keeps it
            "d": roster_worker(dead, "12345", &app, (3, "1")),
            // garbage, but no process has its pid
            "e": {"pid": dead, "procStart": 7},
            // the worker whose session process is the caller
            "f": roster_worker(callers_worker.pid(), &callers_worker.proc_start(), &app,
                (callers.0, &callers.1)),
        },
    }));
    let everyone = vec![
        child_session(&same, &app, SessionSource::SessionFile),
        child_session(&moved, &root, SessionSource::SessionFile),
        child_session(&moved, &app, SessionSource::RosterWorker),
        child_session(&worker, &app, SessionSource::RosterWorker),
    ];
    let callers_session = child_session(&callers_worker, &app, SessionSource::RosterWorker);
    assert_eq!(read_as(&claude, &caller, true), by_pid(everyone.clone()));
    // a caller the process tree doesn't back excludes nothing
    let mut all = everyone.clone();
    all.push(callers_session.clone());
    assert_eq!(read_as(&claude, &caller, false), by_pid(all.clone()));
    assert_eq!(read(&claude), by_pid(all));
    // nor does a worker whose session process only reuses the caller's pid
    let mut doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(claude.0.join("daemon/roster.json")).unwrap(),
    )
    .unwrap();
    doc["workers"]["f"]["replProcStart"] = "1".into();
    claude.roster(&doc);
    let mut all = everyone;
    all.push(callers_session);
    assert_eq!(read_as(&claude, &caller, true), by_pid(all));

    let unparseable = |doc: serde_json::Value| {
        let claude = claude_dir(&ws, "broken");
        let file = claude.roster(&doc);
        match read(&claude) {
            LiveSessions::Unavailable(Unavailable::Unparseable { path, .. }) => {
                assert_eq!(path, self::path(&file), "{doc}");
            }
            other => panic!("{doc}: {other:?}"),
        }
    };
    // no workers field: a format change
    unparseable(serde_json::json!({"proto": 1}));
    // a worker whose liveness can't be told
    unparseable(serde_json::json!({"workers": {"x": {"cwd": "/"}}}));
    // a live worker the reader can't read
    unparseable(serde_json::json!({"workers": {"x": {"pid": worker.pid()}}}));
    // a live worker whose cwd is relative
    unparseable(serde_json::json!({"workers": {"x":
        roster_worker(worker.pid(), &worker.proc_start(), Path::new("app"), (2, "1"))}}));
    // or its worktree, or whose worktree isn't a path
    for worktree in [serde_json::json!("app"), serde_json::json!(7)] {
        let mut doc = roster_worker(worker.pid(), &worker.proc_start(), &app, (2, "1"));
        doc["worktreePath"] = worktree;
        unparseable(serde_json::json!({"workers": {"x": doc}}));
    }
    // a worker in another pid namespace
    let claude = claude_dir(&ws, "foreign-worker");
    let mut foreign = roster_worker(worker.pid(), &worker.proc_start(), &app, (2, "1"));
    foreign["pidDomain"] = "linux:0:pid:[1]".into();
    let roster = claude.roster(&serde_json::json!({"workers": {"x": foreign}}));
    assert_eq!(
        read(&claude),
        LiveSessions::Unavailable(Unavailable::ForeignPidDomain {
            path: path(&roster),
            pid_domain: "linux:0:pid:[1]".into(),
            source: SessionSource::RosterWorker,
        })
    );
}

#[test]
fn the_calling_session_is_excluded_when_it_is_an_ancestor() {
    let ws = FixtureWorkspace::new();
    let claude = claude_dir(&ws, "claude");
    let caller = LiveChild::spawn();
    let other = LiveChild::spawn();
    claude.session(caller.pid(), &caller.proc_start(), &ws.dir("app"));
    claude.session(other.pid(), &other.proc_start(), &ws.root());
    let theirs = child_session(&other, &ws.root(), SessionSource::SessionFile);
    assert_eq!(
        read_as(&claude, &caller, true),
        LiveSessions::Known(vec![theirs.clone()])
    );
    // `CLAUDE_PID` naming a live session that isn't this process's
    // ancestor: anything could have set it, so it's not the caller
    assert_eq!(
        read_as(&claude, &caller, false),
        by_pid(vec![
            child_session(&caller, &ws.dir("app"), SessionSource::SessionFile),
            theirs,
        ])
    );
}

/// Commits once on `branch` in `repo` and pushes it with an upstream, then
/// commits once more locally: ahead 1.
fn ahead_branch(ws: &FixtureWorkspace, repo: &Path, branch: &str) {
    ws.git(repo, &["push", "-q", "origin", &format!("main:{branch}")]);
    ws.git(
        repo,
        &[
            "branch",
            "-q",
            "--track",
            branch,
            &format!("origin/{branch}"),
        ],
    );
    ws.git(repo, &["checkout", "-q", branch]);
    ws.commit(repo, &format!("local-{branch}"));
    ws.git(repo, &["checkout", "-q", "main"]);
    ws.assert_track(repo, branch, "[ahead 1]");
}

/// `app` with Claude Code's worktrees dir ignored, as a user's global
/// excludes would, so a worktree nested in it leaves the primary clean.
fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    support::write(&app, ".git/info/exclude", ".claude/\n");
    app
}

#[test]
fn a_session_marks_the_deepest_checkout_its_cwd_is_in() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // main ahead in the primary
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    // behind, in a worktree nested in the primary
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "feat", "[behind 1]");
    // not in Claude Code's worktrees dir, which any session in `app` holds
    support::write(&app, ".git/info/exclude", "nested/\n");
    let nested = app.join("nested/feat");
    ws.add_worktree(&app, &nested, &["feat"]);
    // ahead, in a worktree beside it, reached through a symlink
    ahead_branch(&ws, &app, "side");
    let side = ws.dir("app-side");
    ws.add_worktree(&app, &side, &["side"]);
    let link = ws.outside("side-link");
    std::os::unix::fs::symlink(&side, &link).unwrap();
    // ahead, checked out nowhere
    ahead_branch(&ws, &app, "loose");
    // another repo, ahead, with nobody in it
    let lib = ws.owned_repo("lib", &[]);
    ws.commit(&lib, "local");
    for c in [&app, &nested, &side, &lib] {
        ws.assert_clean(c);
    }
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(nested.join("src")).unwrap();
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let file = SessionSource::SessionFile;
    let in_primary = session(1, &app.join("src"), file);
    let in_nested = session(2, &nested.join("src"), file);
    let at_root = session(3, &ws.root(), file);
    let outside = session(4, &elsewhere, SessionSource::RosterWorker);
    let via_link = session(5, &link, file);
    let live = LiveSessions::Known(vec![
        in_primary.clone(),
        in_nested.clone(),
        at_root.clone(),
        outside.clone(),
        via_link.clone(),
    ]);
    let run = ws.status_live(&live);
    assert_eq!(
        run.sessions,
        Sessions::Available {
            unscoped: vec![at_root, outside]
        }
    );
    let e = find_entry(&run.entries, "app");
    let busy = |p: &Path| {
        e.checkouts
            .iter()
            .find(|c| c.path == path(p))
            .unwrap_or_else(|| panic!("no checkout {}: {:#?}", p.display(), e.checkouts))
            .busy
            .clone()
    };
    assert_eq!(busy(&app), [in_primary]);
    assert_eq!(busy(&nested), [in_nested]);
    assert_eq!(busy(&side), [via_link]);
    // a busy checkout holds every action on its branch, pushes included
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
    assert_eq!(branch(e, "feat").verdict, held(ff(1), HeldBy::Busy));
    assert_eq!(branch(e, "side").verdict, held(push(1), HeldBy::Busy));
    // and nothing else
    assert_eq!(branch(e, "loose").verdict, Verdict::Act { action: push(1) });
    let lib = find_entry(&run.entries, "lib");
    assert!(lib.checkouts[0].busy.is_empty());
    assert_eq!(
        branch(lib, "main").verdict,
        Verdict::Act { action: push(1) }
    );
}

#[test]
fn cwds_that_do_not_exist_resolve_where_they_do() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    let link = ws.outside("ws-link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();

    let file = SessionSource::SessionFile;
    // a deleted dir, reached through a symlink
    let deleted = session(1, &link.join("app/deleted/deeper"), file);
    // `..` past a dir that doesn't exist, through a symlink
    let climbed = session(2, &link.join("gone/../app/src"), file);
    // `..` out of the checkout, into a dir that doesn't exist
    let left = session(3, &app.join("../app-gone/x"), file);
    let run = ws.status_live(&LiveSessions::Known(vec![
        deleted.clone(),
        climbed.clone(),
        left.clone(),
    ]));
    assert_eq!(
        run.sessions,
        Sessions::Available {
            unscoped: vec![left]
        }
    );
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [deleted, climbed]);
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
}

#[test]
fn checkouts_resolve_through_a_symlinked_workspace_root() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    let link = ws.outside("ws-link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();

    // the primary's path is root-joined: through the link, while the
    // session records where it really is
    let s = session(1, &app, SessionSource::SessionFile);
    let run = ws.status_live_at(&link, &LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].path, path(&link.join("app")));
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
}

#[test]
fn a_session_in_an_unprobed_worktree_holds_its_branch() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ahead_branch(&ws, &app, "hollow");
    // its dir there, its `.git` file gone: it can't be probed
    let hollow = ws.dir("app-hollow");
    ws.add_worktree(&app, &hollow, &["hollow"]);
    std::fs::remove_file(hollow.join(".git")).unwrap();

    // with no session there, only its fast-forward and move would be held:
    // a push only moves refs
    let idle = ws.status_live(&LiveSessions::Known(vec![]));
    let e = find_entry(&idle.entries, "app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    assert_eq!(
        branch(e, "hollow").verdict,
        Verdict::Act { action: push(1) }
    );

    let s = session(9, &hollow, SessionSource::SessionFile);
    let busy = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(busy.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&busy.entries, "app");
    assert_eq!(e.unprobed_worktrees[0].busy, [s]);
    assert_eq!(branch(e, "hollow").verdict, held(push(1), HeldBy::Busy));
}

#[test]
fn a_session_holds_the_worktrees_under_its_claude_worktrees_dir() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    support::write(&app, ".git/info/exclude", ".claude/\nother/\n");
    ws.commit(&app, "local");
    ahead_branch(&ws, &app, "feat");
    ahead_branch(&ws, &app, "hollow");
    ahead_branch(&ws, &app, "side");
    // where Claude Code puts a subagent's worktree, committed in there as a
    // subagent would, while its session file keeps the parent's cwd
    let nested = app.join(".claude/worktrees/x");
    ws.add_worktree(&app, &nested, &["feat"]);
    ws.commit(&nested, "subagent");
    ws.assert_track(&app, "feat", "[ahead 2]");
    // another there, unprobed: its `.git` file gone
    let hollow = app.join(".claude/worktrees/deeper/y");
    ws.add_worktree(&app, &hollow, &["hollow"]);
    std::fs::remove_file(hollow.join(".git")).unwrap();
    // elsewhere in the session's tree, not Claude Code's worktrees dir
    let other = app.join("other/z");
    ws.add_worktree(&app, &other, &["side"]);
    ws.assert_clean(&app);
    let link = ws.outside("app-link");
    std::os::unix::fs::symlink(&app, &link).unwrap();

    // at the primary, as recorded or through a symlink to it
    for cwd in [&app, &link] {
        let s = session(9, cwd, SessionSource::SessionFile);
        let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
        assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
        let e = find_entry(&run.entries, "app");
        let busy = |p: &Path| {
            e.checkouts
                .iter()
                .find(|c| c.path == path(p))
                .unwrap_or_else(|| panic!("no checkout {}: {:#?}", p.display(), e.checkouts))
                .busy
                .clone()
        };
        assert_eq!(busy(&app), std::slice::from_ref(&s));
        assert_eq!(busy(&nested), std::slice::from_ref(&s));
        assert!(busy(&other).is_empty());
        assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
        assert_eq!(e.unprobed_worktrees[0].busy, std::slice::from_ref(&s));
        assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
        assert_eq!(branch(e, "feat").verdict, held(push(2), HeldBy::Busy));
        assert_eq!(branch(e, "hollow").verdict, held(push(1), HeldBy::Busy));
        assert_eq!(branch(e, "side").verdict, Verdict::Act { action: push(1) });
    }
}

/// The checkouts of `e` a live session holds, probed or not, by path.
fn busy_checkouts(e: &EntryStatus) -> Vec<&str> {
    let mut busy: Vec<&str> = e
        .checkouts
        .iter()
        .filter(|c| !c.busy.is_empty())
        .map(|c| c.path.as_str())
        .chain(
            e.unprobed_worktrees
                .iter()
                .filter(|u| !u.busy.is_empty())
                .map(|u| u.worktree.path.as_str()),
        )
        .collect();
    busy.sort_unstable();
    busy
}

/// Paths as `busy_checkouts` lists them.
fn paths(ps: &[&Path]) -> Vec<String> {
    let mut all: Vec<String> = ps.iter().map(|p| path(p)).collect();
    all.sort_unstable();
    all
}

#[test]
fn a_session_anywhere_in_a_repo_holds_the_worktrees_claude_code_roots_at_its_primary() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    for b in ["feat", "side", "gamma"] {
        ahead_branch(&ws, &app, b);
    }
    let nested = app.join(".claude/worktrees/x");
    ws.add_worktree(&app, &nested, &["feat"]);
    let wt = ws.dir("app-wt");
    ws.add_worktree(&app, &wt, &["side"]);
    // under the linked worktree's own `.claude/worktrees/`: Claude Code
    // roots a session's worktrees at the primary from there too, so this is
    // no subagent's
    let under_wt = wt.join(".claude/worktrees/y");
    ws.add_worktree(&app, &under_wt, &["gamma"]);
    std::fs::create_dir_all(app.join("crates/sub")).unwrap();
    std::fs::create_dir_all(wt.join("src")).unwrap();
    for c in [&app, &nested, &wt, &under_wt] {
        ws.assert_clean(c);
    }

    let cases: [(&Path, &[&Path], [&str; 2]); 2] = [
        // a subdir of the primary: the primary's worktrees
        (&app.join("crates/sub"), &[&app, &nested], ["main", "feat"]),
        // a linked worktree of it: the primary's too, not its own
        (&wt.join("src"), &[&wt, &nested], ["side", "feat"]),
    ];
    for (cwd, busy, held_branches) in cases {
        let s = session(9, cwd, SessionSource::SessionFile);
        let run = ws.status_live(&LiveSessions::Known(vec![s]));
        assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
        let e = find_entry(&run.entries, "app");
        assert_eq!(busy_checkouts(e), paths(busy), "{}", cwd.display());
        for b in ["main", "feat", "side", "gamma"] {
            let expected = if held_branches.contains(&b) {
                held(push(1), HeldBy::Busy)
            } else {
                Verdict::Act { action: push(1) }
            };
            assert_eq!(branch(e, b).verdict, expected, "{b} from {}", cwd.display());
        }
    }
}

#[test]
fn a_separate_git_dir_repos_worktrees_are_rooted_where_claude_code_roots_them() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app-git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    support::write(&git_dir, "info/exclude", ".claude/\n");
    ws.commit(&app, "local");
    for b in ["feat", "side", "gamma"] {
        ahead_branch(&ws, &app, b);
    }
    // a session in the primary gets its worktrees under the primary, one in
    // a linked worktree under the git dir itself: the common dir isn't
    // named `.git`
    let in_primary = app.join(".claude/worktrees/x");
    ws.add_worktree(&app, &in_primary, &["feat"]);
    let wt = ws.dir("app-wt");
    ws.add_worktree(&app, &wt, &["side"]);
    let in_git_dir = git_dir.join(".claude/worktrees/q");
    ws.add_worktree(&app, &in_git_dir, &["gamma"]);

    let cases: [(&Path, &[&Path]); 2] = [
        // the git dir's worktrees too: the repo's root is held for any
        // session in it, wherever Claude Code would root this one's
        (&app, &[&app, &in_primary, &in_git_dir]),
        (&wt, &[&wt, &in_git_dir]),
    ];
    for (cwd, busy) in cases {
        let s = session(9, cwd, SessionSource::SessionFile);
        let run = ws.status_live(&LiveSessions::Known(vec![s]));
        let e = find_entry(&run.entries, "app");
        assert_eq!(busy_checkouts(e), paths(busy), "{}", cwd.display());
    }
}

#[test]
fn a_session_in_a_moved_worktree_holds_the_worktrees_under_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    // its link back no longer names it, so Claude Code roots its worktrees
    // at the moved worktree itself; made from there, as it would
    ahead_branch(&ws, &app, "gamma");
    let q = moved.join(".claude/worktrees/q");
    ws.add_worktree(&moved, &q, &["gamma"]);

    let s = session(9, &moved, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(busy_checkouts(e), paths(&[&wt, &q]));
    assert_eq!(branch(e, "feat").verdict, held(push(2), HeldBy::Busy));
    assert_eq!(branch(e, "gamma").verdict, held(push(1), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
}

#[test]
fn a_session_is_placed_where_its_process_is() {
    let mut ws = FixtureWorkspace::new();
    let (_, wt, _) = app_with_a_feat_worktree(&mut ws);
    std::fs::create_dir(wt.join("src")).unwrap();
    let claude = claude_dir(&ws, "claude");
    // launched at the workspace root, since moved into the worktree, as
    // Claude Code moves into one it enters
    let entered = LiveChild::spawn_in(&wt.join("src"));
    claude.session(entered.pid(), &entered.proc_start(), &ws.root());
    // where its session file says: nothing more to know
    let stayed = LiveChild::spawn_in(&ws.root());
    claude.session(stayed.pid(), &stayed.proc_start(), &ws.root());
    // in a dir since removed: nothing there to hold
    let gone = ws.outside("gone");
    std::fs::create_dir(&gone).unwrap();
    let removed = LiveChild::spawn_in(&gone);
    std::fs::remove_dir(&gone).unwrap();
    claude.session(removed.pid(), &removed.proc_start(), &ws.root());
    let file = SessionSource::SessionFile;
    let entered_session = Session {
        process_cwd: Some(path(&wt.join("src"))),
        ..live_session(&entered, &ws.root(), file)
    };
    let live = read(&claude);
    assert_eq!(
        live,
        by_pid(vec![
            entered_session.clone(),
            live_session(&stayed, &ws.root(), file),
            live_session(&removed, &ws.root(), file),
        ])
    );
    let run = ws.status_live(&live);
    let LiveSessions::Known(all) = live else {
        unreachable!()
    };
    let unscoped: Vec<Session> = all.into_iter().filter(|s| s.pid != entered.pid()).collect();
    assert_eq!(run.sessions, Sessions::Available { unscoped });
    let e = find_entry(&run.entries, "app");
    assert_eq!(busy_checkouts(e), paths(&[&wt]));
    assert_eq!(
        e.checkouts
            .iter()
            .find(|c| c.path == path(&wt))
            .unwrap()
            .busy,
        [entered_session]
    );
    assert_eq!(branch(e, "feat").verdict, held(push(1), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
}

#[test]
fn a_roster_worker_is_placed_in_its_worktree() {
    let mut ws = FixtureWorkspace::new();
    let (_, wt, _) = app_with_a_feat_worktree(&mut ws);
    let claude = claude_dir(&ws, "claude");
    let worker = LiveChild::spawn_in(&ws.root());
    let mut doc = roster_worker(worker.pid(), &worker.proc_start(), &ws.root(), (1, "1"));
    doc["worktreePath"] = path(&wt).into();
    claude.roster(&serde_json::json!({"workers": {"w": doc}}));
    let placed = Session {
        worktree: Some(path(&wt)),
        ..live_session(&worker, &ws.root(), SessionSource::RosterWorker)
    };
    let live = read(&claude);
    assert_eq!(live, LiveSessions::Known(vec![placed.clone()]));
    let run = ws.status_live(&live);
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(busy_checkouts(e), paths(&[&wt]));
    assert_eq!(branch(e, "feat").verdict, held(push(1), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });

    // its session file at the same cwd wins, and keeps the worktree
    claude.session(worker.pid(), &worker.proc_start(), &ws.root());
    assert_eq!(
        read(&claude),
        LiveSessions::Known(vec![Session {
            source: SessionSource::SessionFile,
            ..placed
        }])
    );
}

/// `app` with `main` ahead in the primary and `feat` ahead in a linked
/// worktree at `app-feat`; returns the primary, the worktree, and its own
/// git dir.
fn app_with_a_feat_worktree(ws: &mut FixtureWorkspace) -> (PathBuf, PathBuf, PathBuf) {
    let app = app(ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ahead_branch(ws, &app, "feat");
    let wt = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &wt, &["feat"]);
    (app, wt, admin)
}

/// A session at `cwd` is attributed to `app`'s worktree at `checkout`, by
/// the `.git` it walks up to, and to nothing else: `feat`'s push is held as
/// busy while the primary's acts.
fn assert_feat_busy(ws: &FixtureWorkspace, cwd: &Path, checkout: &Path, commits: u32) {
    let s = session(9, cwd, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    let busy: Vec<(&str, &[Session])> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.busy.as_slice()))
        .chain(
            e.unprobed_worktrees
                .iter()
                .map(|u| (u.worktree.path.as_str(), u.busy.as_slice())),
        )
        .filter(|(_, busy)| !busy.is_empty())
        .collect();
    assert_eq!(busy, [(path(checkout).as_str(), std::slice::from_ref(&s))]);
    assert_eq!(branch(e, "feat").verdict, held(push(commits), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
}

/// Moves `app`'s worktree `wt` to `to` with a plain `mv`, and checks git
/// still works there, on its branch, committing to it: `feat` ends ahead 2.
fn move_by_hand(ws: &FixtureWorkspace, app: &Path, wt: &Path, to: &Path) {
    std::fs::rename(wt, to).unwrap();
    ws.assert_head(to, Some("feat"));
    ws.commit(to, "moved");
    ws.assert_track(app, "feat", "[ahead 2]");
    // git doesn't repair the move
    let record = ws.worktree_record(app, wt);
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );
}

#[test]
fn a_session_in_a_worktree_moved_by_hand_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
    // out of the workspace root, where the scan wouldn't see it
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    std::fs::create_dir(moved.join("src")).unwrap();
    // the probe knows it only by its old path, gone
    let run = ws.status_live(&LiveSessions::Known(vec![]));
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    assert_eq!(e.unprobed_worktrees[0].worktree.why, UnprobedWhy::Prunable);
    assert_eq!(e.unprobed_worktrees[0].prune, Some(Prune::Safe));
    // with nobody in it, its push acts
    assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(2) });

    // a session in it, or deeper, finds its `.git`, which names the
    // worktree's own git dir
    assert_feat_busy(&ws, &moved, &wt, 2);
    assert_feat_busy(&ws, &moved.join("src"), &wt, 2);
    // a repo nested in it, or a `.git` naming nothing, is passed over: git
    // itself would find the nested repo, but the session sits in the
    // worktree's files all the same
    let vendor = moved.join("vendor");
    ws.git(&moved, &["init", "-q", "vendor"]);
    assert!(vendor.join(".git").is_dir());
    assert_feat_busy(&ws, &vendor, &wt, 2);
    let dangling = moved.join("dangling");
    support::write(&dangling, ".git", "gitdir: ../nowhere\n");
    assert_feat_busy(&ws, &dangling, &wt, 2);
}

#[test]
fn a_worktree_moved_into_another_checkout_is_busy_with_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
    let lib = ws.owned_repo("lib", &[]);
    ws.commit(&lib, "local");
    ws.assert_track(&lib, "main", "[ahead 1]");
    // into `lib`'s tree: the path puts a session there in `lib`
    let moved = lib.join("app-feat");
    move_by_hand(&ws, &app, &wt, &moved);

    let s = session(9, &moved, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    // and its `.git` in `app`'s worktree: both hold, pushes included
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.unprobed_worktrees[0].worktree.path, path(&wt));
    assert_eq!(e.unprobed_worktrees[0].busy, std::slice::from_ref(&s));
    assert!(e.checkouts.iter().all(|c| c.busy.is_empty()));
    assert_eq!(branch(e, "feat").verdict, held(push(2), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    let lib = find_entry(&run.entries, "lib");
    assert_eq!(lib.checkouts[0].busy, [s]);
    assert_eq!(branch(lib, "main").verdict, held(push(1), HeldBy::Busy));
}

/// Copies `from` to `to` as `cp -a` does, symlinks and modes kept.
fn copy_tree(from: &Path, to: &Path) {
    let out = std::process::Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_session_in_a_copy_of_a_worktree_is_attributed_to_it() {
    for inside in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
        // a copy's `.git` still names the worktree's own git dir, so
        // committing there moves its branch
        let copy = if inside {
            ws.dir("app-copy")
        } else {
            ws.outside("app-copy")
        };
        copy_tree(&wt, &copy);
        ws.assert_head(&copy, Some("feat"));
        ws.commit(&copy, "copied");
        ws.assert_track(&app, "feat", "[ahead 2]");
        // the worktree itself stays where git lists it
        ws.assert_head(&wt, Some("feat"));

        assert_feat_busy(&ws, &copy, &wt, 2);
    }
}

#[test]
fn a_session_in_a_copy_of_a_separate_git_dir_primary_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app-git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    assert!(app.join(".git").is_file());
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    let copy = ws.outside("app-copy");
    copy_tree(&app, &copy);
    ws.assert_head(&copy, Some("main"));

    let s = session(9, &copy, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
}

#[test]
fn a_gone_worktree_with_nobody_in_it_holds_only_what_a_push_does_not_touch() {
    for locked in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
        if locked {
            // as on media that's been unmounted
            ws.git(&app, &["worktree", "lock", wt.to_str().unwrap()]);
        }
        std::fs::remove_dir_all(&wt).unwrap();
        let why = if locked {
            UnprobedWhy::Missing
        } else {
            UnprobedWhy::Prunable
        };
        // a session elsewhere changes nothing
        let at_root = session(9, &ws.root(), SessionSource::SessionFile);
        let run = ws.status_live(&LiveSessions::Known(vec![at_root.clone()]));
        assert_eq!(
            run.sessions,
            Sessions::Available {
                unscoped: vec![at_root]
            }
        );
        let e = find_entry(&run.entries, "app");
        assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
        assert_eq!(e.unprobed_worktrees[0].worktree.why, why);
        assert_eq!(
            branch(e, "feat").verdict,
            Verdict::Act { action: push(1) },
            "{why:?}"
        );
        assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    }
}

#[test]
fn a_session_in_a_worktree_whose_gitdir_names_no_path_is_attributed_to_it() {
    for lost in ["missing", "empty", "unreadable"] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, admin) = app_with_a_feat_worktree(&mut ws);
        let gitdir = admin.join("gitdir");
        let _unseal = match lost {
            "missing" => {
                std::fs::remove_file(&gitdir).unwrap();
                None
            }
            "empty" => {
                std::fs::write(&gitdir, "").unwrap();
                None
            }
            _ => {
                let Some(unseal) = support::seal(&gitdir, 0o000) else {
                    return;
                };
                Some(unseal)
            }
        };
        // git drops it from its list, but it still works there
        let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
        assert!(!list.contains(&path(&wt)), "{lost}: {list}");
        ws.assert_head(&wt, Some("feat"));

        // its only path is its own git dir, which the `.git` a session in
        // it walks up to names
        assert_feat_busy(&ws, &wt, &admin, 1);
        let run = ws.status_live(&LiveSessions::Known(vec![]));
        let e = find_entry(&run.entries, "app");
        assert_eq!(
            e.unprobed_worktrees[0].worktree.path,
            path(&admin),
            "{lost}"
        );
        assert!(e.needs_human.is_empty(), "{lost}: {:?}", e.needs_human);
        assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(1) });
    }
}

/// git's own limit on a `.git` file (`read_gitfile_gently`).
const MAX_GITFILE_BYTES: usize = 1024 * 1024;

/// Writes `dir/.git` with raw `content`, creating `dir`.
fn write_dot_git(dir: &Path, content: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".git"), content).unwrap();
}

/// A gitfile naming `admin`, padded with line breaks to `len` bytes.
fn padded_gitfile(admin: &Path, len: usize) -> Vec<u8> {
    let mut bytes = format!("gitdir: {}", admin.display()).into_bytes();
    bytes.resize(len, b'\n');
    bytes
}

/// Whether git run in `dir` stops with an error, finding no repo it can use.
fn git_stops_at(ws: &FixtureWorkspace, dir: &Path) -> bool {
    !ws.git_output(dir, &["rev-parse", "--absolute-git-dir"])
        .status
        .success()
}

#[test]
fn a_dot_git_git_cannot_use_is_passed_over() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, admin) = app_with_a_feat_worktree(&mut ws);
    // out of the workspace root, so only the walk finds it
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    // under the moved worktree, and under no checkout at all
    let loose = ws.outside("loose");
    for parent in [&moved, &loose] {
        // each `.git` git stops at with an error: garbage, over its size
        // limit, naming nothing, or naming a path through a symlink loop
        let garbage = parent.join("garbage");
        write_dot_git(&garbage, b"not a gitfile\n");
        let oversize = parent.join("oversize");
        write_dot_git(&oversize, &padded_gitfile(&admin, MAX_GITFILE_BYTES + 1));
        let nowhere = parent.join("nowhere");
        write_dot_git(&nowhere, b"gitdir: /nonexistent-git-dir\n");
        let looped = parent.join("looped");
        std::fs::create_dir(&looped).unwrap();
        std::os::unix::fs::symlink(".git", looped.join(".git")).unwrap();
        let through_loop = parent.join("through-loop");
        write_dot_git(&through_loop, b"gitdir: ../looped/.git/x\n");
        for dir in [&garbage, &oversize, &nowhere, &through_loop] {
            assert!(git_stops_at(&ws, dir), "{}", dir.display());
        }
        // and one it passes over: the loop itself
        let dirs = [&garbage, &oversize, &nowhere, &looped, &through_loop];
        if parent == &moved {
            assert_eq!(
                ws.git(&looped, &["rev-parse", "--absolute-git-dir"]),
                path(&admin)
            );
            // the moved worktree's `.git` above them is the walk's
            for dir in dirs {
                assert_feat_busy(&ws, dir, &wt, 2);
            }
        } else {
            for dir in dirs {
                assert!(git_stops_at(&ws, dir), "{}", dir.display());
                let s = session(9, dir, SessionSource::SessionFile);
                let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
                assert_eq!(run.sessions, Sessions::Available { unscoped: vec![s] });
                let e = find_entry(&run.entries, "app");
                assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(2) });
            }
        }
    }
    // one that can't be read, or a `.git` dir that can't: git stops at the
    // gitfile and passes over the dir. Last: sealing binds only a non-root
    // user
    let sealed = moved.join("sealed");
    write_dot_git(&sealed, format!("gitdir: {}\n", admin.display()).as_bytes());
    let sealed_dir = moved.join("sealed-dir");
    std::fs::create_dir_all(sealed_dir.join(".git")).unwrap();
    let Some(_unseal) = support::seal(&sealed.join(".git"), 0o000) else {
        return;
    };
    let Some(_unseal_dir) = support::seal(&sealed_dir.join(".git"), 0o000) else {
        return;
    };
    assert!(git_stops_at(&ws, &sealed));
    assert_eq!(
        ws.git(&sealed_dir, &["rev-parse", "--absolute-git-dir"]),
        path(&admin)
    );
    assert_feat_busy(&ws, &sealed, &wt, 2);
    assert_feat_busy(&ws, &sealed_dir, &wt, 2);
}

/// Rewrites the `.git` of `app`'s worktree `wt` as `content`, moves it by
/// hand to a dir outside the workspace root — committing there through it,
/// so git itself reads it as a gitfile — and checks a session there is
/// attributed to the worktree.
fn assert_gitfile_followed(content: &dyn Fn(&Path) -> Vec<u8>) {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, admin) = app_with_a_feat_worktree(&mut ws);
    std::fs::write(wt.join(".git"), content(&admin)).unwrap();
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    assert_eq!(
        ws.git(&moved, &["rev-parse", "--absolute-git-dir"]),
        path(&admin)
    );
    assert_feat_busy(&ws, &moved, &wt, 2);
}

#[test]
fn a_gitfile_is_followed_where_git_follows_it() {
    // a C string: what follows a NUL is ignored
    assert_gitfile_followed(&|admin| {
        let mut bytes = format!("gitdir: {}", admin.display()).into_bytes();
        bytes.extend(b"\0junk\n");
        bytes
    });
    // line breaks trimmed however many, up to git's size limit
    assert_gitfile_followed(&|admin| padded_gitfile(admin, 100_000));
    assert_gitfile_followed(&|admin| padded_gitfile(admin, MAX_GITFILE_BYTES));
}

#[test]
fn a_gitfile_naming_a_path_that_is_not_utf8_is_followed() {
    use std::os::unix::ffi::OsStrExt as _;
    let probe = tempfile::tempdir().unwrap();
    let name = std::ffi::OsStr::from_bytes(b"git-\xff");
    if std::fs::create_dir(probe.path().join(name)).is_err() {
        eprintln!("skipped: the filesystem refuses a name that isn't UTF-8");
        return;
    }
    assert_gitfile_followed(&|admin| {
        // the common dir by a symlink whose name isn't UTF-8
        let common = admin.parent().unwrap().parent().unwrap();
        let link = common.parent().unwrap().parent().unwrap().join(name);
        std::os::unix::fs::symlink(common, &link).unwrap();
        let mut bytes = b"gitdir: ".to_vec();
        bytes.extend(link.as_os_str().as_bytes());
        bytes.extend(b"/worktrees/");
        bytes.extend(admin.file_name().unwrap().as_bytes());
        bytes.push(b'\n');
        bytes
    });
}

#[test]
fn a_dot_git_symlinked_to_a_checkouts_git_dir_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    // outside every checkout, its `.git` the primary's git dir
    let lnk = ws.outside("lnk");
    std::fs::create_dir(&lnk).unwrap();
    std::os::unix::fs::symlink(app.join(".git"), lnk.join(".git")).unwrap();
    ws.git(&lnk, &["commit", "-q", "--allow-empty", "-m", "via-link"]);
    ws.assert_track(&app, "main", "[ahead 2]");

    let s = session(9, &lnk, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(2), HeldBy::Busy));
}

#[test]
fn a_session_inside_a_git_dir_works_in_its_checkout() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, admin) = app_with_a_feat_worktree(&mut ws);
    // git takes the git dir for the repo, and moves the branch its HEAD is
    // on from there
    assert_eq!(ws.git(&admin, &["rev-parse", "--git-dir"]), ".");
    let commit = ws.git(
        &admin,
        &["commit-tree", "HEAD:", "-p", "HEAD", "-m", "inside"],
    );
    ws.git(&admin, &["update-ref", "HEAD", &commit]);
    ws.assert_track(&app, "feat", "[ahead 2]");

    let s = session(9, &admin, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    // the worktree by its git dir, the primary by the path it sits in
    let busy: Vec<(&str, usize)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.busy.len()))
        .collect();
    assert_eq!(busy, [(path(&app).as_str(), 1), (path(&wt).as_str(), 1)]);
    assert_eq!(branch(e, "feat").verdict, held(push(2), HeldBy::Busy));
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
}

#[test]
fn a_session_inside_a_separate_git_dir_works_in_its_checkout() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app-git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    // below it, outside every checkout
    let refs = git_dir.join("refs");
    assert_eq!(
        ws.git(&refs, &["rev-parse", "--absolute-git-dir"]),
        path(&git_dir)
    );

    let s = session(9, &refs, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(1), HeldBy::Busy));
}

/// `app` with `main` ahead in the primary and `other` ahead, checked out
/// nowhere; returns the primary.
fn app_with_other(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = app(ws);
    ws.commit(&app, "local");
    ahead_branch(ws, &app, "other");
    app
}

/// A session at `cwd` works through the unlisted git dir `git_dir`, whose
/// HEAD is `head`: `app`'s branches it may be on are held as busy unknown,
/// the rest act, and the reason names it.
fn assert_unlisted(
    ws: &FixtureWorkspace,
    cwd: &Path,
    git_dir: &Path,
    head: UnprobedHead,
    held_ones: &[(&str, u32)],
    acting: &[(&str, u32)],
) {
    let s = session(9, cwd, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert!(e.checkouts.iter().all(|c| c.busy.is_empty()));
    assert_eq!(
        e.needs_human,
        [NeedsHuman::UnlistedGitDir {
            git_dir: path(&git_dir.canonicalize().unwrap()),
            head,
            busy: vec![s],
        }]
    );
    for &(name, commits) in held_ones {
        assert_eq!(
            branch(e, name).verdict,
            held(push(commits), HeldBy::BusyUnknown),
            "{name}"
        );
    }
    for &(name, commits) in acting {
        assert_eq!(
            branch(e, name).verdict,
            Verdict::Act {
                action: push(commits)
            },
            "{name}"
        );
    }
}

#[test]
fn a_session_in_a_hand_made_git_dir_holds_the_branch_it_is_on() {
    for relative in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let app = app_with_other(&mut ws);
        // a `.git` dir with a `commondir`, as a worktree's git dir has, that
        // no worktree list names
        let hand = ws.outside("hand");
        let git_dir = hand.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let common = if relative {
            "../../ws/app/.git\n".to_owned()
        } else {
            format!("{}\n", app.join(".git").display())
        };
        std::fs::write(git_dir.join("commondir"), common).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        ws.git(&hand, &["reset", "-q"]);
        ws.commit(&hand, "hand");
        ws.assert_track(&app, "other", "[ahead 2]");
        assert!(!ws.git(&app, &["worktree", "list"]).contains("hand"));

        let on_other = UnprobedHead::Branch {
            name: "other".into(),
        };
        assert_unlisted(
            &ws,
            &hand,
            &git_dir,
            on_other.clone(),
            &[("other", 2)],
            &[("main", 1)],
        );
        // deeper in its files too
        std::fs::create_dir(hand.join("src")).unwrap();
        assert_unlisted(
            &ws,
            &hand.join("src"),
            &git_dir,
            on_other,
            &[("other", 2)],
            &[("main", 1)],
        );
    }
}

#[test]
fn a_hand_made_git_dirs_head_decides_what_it_holds() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let hand = ws.outside("hand");
    let git_dir = hand.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        git_dir.join("commondir"),
        format!("{}\n", app.join(".git").display()),
    )
    .unwrap();
    // detached: no branch moves
    let commit = ws.git(&app, &["rev-parse", "main"]);
    std::fs::write(git_dir.join("HEAD"), format!("{commit}\n")).unwrap();
    assert_unlisted(
        &ws,
        &hand,
        &git_dir,
        UnprobedHead::Detached { commit },
        &[],
        &[("main", 1), ("other", 1)],
    );
    // unknown: any branch might
    std::fs::write(git_dir.join("HEAD"), "ref: refs/tags/v1\n").unwrap();
    assert_unlisted(
        &ws,
        &hand,
        &git_dir,
        UnprobedHead::Unknown,
        &[("main", 1), ("other", 1)],
        &[],
    );
}

/// A hand-made git dir at `hand` whose `commondir` is `commondir` and whose
/// `HEAD` is `head`, its index read from the commit git finds through them.
fn hand_made(ws: &FixtureWorkspace, hand: &Path, commondir: &[u8], head: &[u8]) -> PathBuf {
    let git_dir = hand.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("commondir"), commondir).unwrap();
    std::fs::write(git_dir.join("HEAD"), head).unwrap();
    ws.git(hand, &["reset", "-q"]);
    git_dir
}

#[test]
fn a_hand_made_git_dirs_commondir_and_head_are_read_as_git_reads_them() {
    // past git's gitfile limit, which holds for neither file: git reads each
    // whole, however large
    let past_any_limit = |mut bytes: Vec<u8>| {
        bytes.resize(bytes.len() + MAX_GITFILE_BYTES + 64 * 1024, b'x');
        bytes
    };
    let on_other = b"ref: refs/heads/other\n".to_vec();
    // each a C string: cut at the first NUL
    let layouts: [(&str, &[u8], Vec<u8>); 3] = [
        (
            "HEAD cut at a NUL",
            b"\n",
            b"ref: refs/heads/other\0junk\n".to_vec(),
        ),
        (
            "HEAD read to a NUL past any size",
            b"\n",
            past_any_limit(b"ref: refs/heads/other\0".to_vec()),
        ),
        (
            "commondir read to a NUL past any size",
            &past_any_limit(b"\0".to_vec()),
            on_other,
        ),
    ];
    for (layout, commondir_tail, head) in layouts {
        let mut ws = FixtureWorkspace::new();
        let app = app_with_other(&mut ws);
        let hand = ws.outside("hand");
        let mut common = app.join(".git").into_os_string().into_encoded_bytes();
        common.extend_from_slice(commondir_tail);
        let git_dir = hand_made(&ws, &hand, &common, &head);
        // git itself reads both so, and commits to `other` through them
        assert_eq!(
            ws.git(&hand, &["symbolic-ref", "HEAD"]),
            "refs/heads/other",
            "{layout}"
        );
        ws.commit(&hand, "hand");
        ws.assert_track(&app, "other", "[ahead 2]");

        assert_unlisted(
            &ws,
            &hand,
            &git_dir,
            UnprobedHead::Branch {
                name: "other".into(),
            },
            &[("other", 2)],
            &[("main", 1)],
        );
    }
}

#[test]
fn a_session_in_a_git_dir_with_only_its_branches_linked_holds_the_branch_it_is_on() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let linked = ws.outside("linked");
    let git_dir = linked.join(".git");
    // a real `refs` with `heads` linked into the original's, and no
    // `commondir`: git keeps its branches there all the same
    std::fs::create_dir_all(git_dir.join("refs")).unwrap();
    for shared in ["config", "objects", "refs/heads"] {
        std::os::unix::fs::symlink(app.join(".git").join(shared), git_dir.join(shared)).unwrap();
    }
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
    ws.git(&linked, &["reset", "-q"]);
    ws.commit(&linked, "linked");
    ws.assert_track(&app, "other", "[ahead 2]");
    assert!(!git_dir.join("refs").is_symlink());

    assert_unlisted(
        &ws,
        &linked,
        &git_dir,
        UnprobedHead::Branch {
            name: "other".into(),
        },
        &[("other", 2)],
        &[("main", 1)],
    );
}

#[test]
fn a_session_in_a_git_new_workdir_holds_the_branch_it_is_on() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let new = ws.outside("new-workdir");
    let script = Path::new("/usr/share/doc/git/contrib/workdir/git-new-workdir");
    if script.is_file() {
        let out = ws
            .command("sh", &ws.root())
            .arg(script)
            .arg(&app)
            .arg(&new)
            .arg("other")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    } else {
        // as the script makes one: the shared parts linked, HEAD copied
        let git_dir = new.join(".git");
        std::fs::create_dir_all(git_dir.join("logs")).unwrap();
        for shared in [
            "config",
            "refs",
            "logs/refs",
            "objects",
            "info",
            "hooks",
            "packed-refs",
        ] {
            std::os::unix::fs::symlink(app.join(".git").join(shared), git_dir.join(shared))
                .unwrap();
        }
        std::fs::copy(app.join(".git/HEAD"), git_dir.join("HEAD")).unwrap();
        ws.git(&new, &["checkout", "-q", "-f", "other"]);
    }
    assert!(new.join(".git").join("refs").is_symlink());
    assert!(!new.join(".git").join("commondir").exists());
    ws.commit(&new, "new-workdir");
    ws.assert_track(&app, "other", "[ahead 2]");

    assert_unlisted(
        &ws,
        &new,
        &new.join(".git"),
        UnprobedHead::Branch {
            name: "other".into(),
        },
        &[("other", 2)],
        &[("main", 1)],
    );
}

#[test]
fn a_bare_repos_main_worktree_holds_nothing() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    // the registry's dir is a linked worktree of a bare clone beside it
    ws.declare_repo("app", "app", "");
    let bare = ws.clone_owned("app.git", "app", &["--bare"]);
    ws.git(
        &bare,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    ws.git(&bare, &["fetch", "-q", "origin"]);
    ws.git(
        &bare,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    );
    let app = ws.dir("app");
    ws.add_worktree(&bare, &app, &["-b", "feat"]);
    let oid = ws.commit(&app, "local");
    ws.git(&bare, &["update-ref", "refs/heads/main", &oid]);
    ws.assert_track(&app, "main", "[ahead 1]");
    // git lists the bare main worktree with no HEAD, yet names it as where
    // HEAD's branch is checked out
    let list = ws.git(&bare, &["worktree", "list", "--porcelain"]);
    assert!(
        list.starts_with(&format!("worktree {}\nbare\n", path(&bare))),
        "{list}"
    );
    assert_eq!(
        ws.git(
            &app,
            &[
                "for-each-ref",
                "--format=%(worktreepath)",
                "refs/heads/main"
            ]
        ),
        path(&bare)
    );

    let run = ws.status_live(&LiveSessions::Known(vec![]));
    let e = find_entry(&run.entries, "app");
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
}

#[test]
fn an_unresolvable_checkout_holds_only_the_branch_checked_out_there() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    // a worktree in a dir that will be sealed
    ahead_branch(&ws, &app, "sealed");
    let sealed = ws.outside("sealed");
    std::fs::create_dir(&sealed).unwrap();
    let wt = sealed.join("app-wt");
    ws.add_worktree(&app, &wt, &["sealed"]);
    let lib = ws.owned_repo("lib", &[]);
    ws.commit(&lib, "local");
    ws.assert_track(&lib, "main", "[ahead 1]");
    let Some(_unseal) = support::seal(&sealed, 0o000) else {
        return;
    };
    // the worktree's dir can't even be looked up
    assert_eq!(
        std::fs::symlink_metadata(&wt).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let unresolvable = NeedsHuman::CheckoutUnresolvable {
        checkout: path(&wt),
        path: path(&wt),
        error: "Permission denied (os error 13)".into(),
    };
    // the same whether or not some session is live: the worktree's branch
    // held, its push included; `app`'s other branch acts, and so does `lib`
    let held_only_sealed = |run: &StatusRun| {
        let e = find_entry(&run.entries, "app");
        assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
        assert_eq!(e.unprobed_worktrees[0].worktree.path, path(&wt));
        assert_eq!(e.needs_human, std::slice::from_ref(&unresolvable));
        assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
        assert_eq!(
            branch(e, "sealed").verdict,
            held(push(1), HeldBy::BusyUnknown)
        );
        let lib = find_entry(&run.entries, "lib");
        assert!(lib.needs_human.is_empty(), "{:?}", lib.needs_human);
        assert_eq!(
            branch(lib, "main").verdict,
            Verdict::Act { action: push(1) }
        );
    };
    let idle = ws.status_live(&LiveSessions::Known(vec![]));
    assert_eq!(idle.sessions, Sessions::Available { unscoped: vec![] });
    held_only_sealed(&idle);
    let at_root = session(9, &ws.root(), SessionSource::SessionFile);
    let live = ws.status_live(&LiveSessions::Known(vec![at_root.clone()]));
    assert_eq!(
        live.sessions,
        Sessions::Available {
            unscoped: vec![at_root]
        }
    );
    held_only_sealed(&live);

    // a session inside it can't be resolved either: detection fails closed
    let inside = session(9, &wt.join("src"), SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![inside]));
    assert_eq!(
        run.sessions,
        Sessions::Unavailable {
            reason: Unavailable::Unreadable {
                path: path(&wt),
                error: "Permission denied (os error 13)".into(),
            }
        }
    );
    assert_eq!(
        branch(find_entry(&run.entries, "lib"), "main").verdict,
        held(push(1), HeldBy::BusyUnknown)
    );
}

#[test]
fn one_checkout_of_two_entries_is_busy_for_both() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // `app-next` is `app`'s linked worktree, and an entry of its own
    ahead_branch(&ws, &app, "next");
    let next = ws.dir("app-next");
    ws.add_worktree(&app, &next, &["next"]);
    ws.declare_repo("app-next", "app", "dir = \"app-next\"\nbranch = \"next\"");
    ws.assert_clean(&next);

    let s = session(9, &next, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    for key in ["app", "app-next"] {
        let e = find_entry(&run.entries, key);
        let c = e.checkouts.iter().find(|c| c.path == path(&next)).unwrap();
        assert_eq!(c.busy, std::slice::from_ref(&s), "{key}");
        assert_eq!(
            branch(e, "next").verdict,
            held(push(1), HeldBy::Busy),
            "{key}"
        );
    }
}

#[test]
fn a_worktree_a_session_works_in_is_never_removable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.git(&app, &["push", "-q", "origin", "main:old"]);
    ws.git(&app, &["branch", "-q", "--track", "old", "origin/old"]);
    ws.upstream_delete_branch("app", "old");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "old", "[gone]");
    let old = ws.dir("app-old");
    ws.add_worktree(&app, &old, &["old"]);
    ws.assert_clean(&old);
    let cleanup = |removable: Option<String>| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree: removable,
    };

    let idle = ws.status_live(&LiveSessions::Known(vec![]));
    assert_eq!(
        branch(find_entry(&idle.entries, "app"), "old").verdict,
        cleanup(Some(path(&old)))
    );
    let busy = ws.status_live(&LiveSessions::Known(vec![session(
        9,
        &old,
        SessionSource::SessionFile,
    )]));
    assert_eq!(
        branch(find_entry(&busy.entries, "app"), "old").verdict,
        cleanup(None)
    );
    // nor when a session there can't be ruled out
    let unknown = ws.status_live(&LiveSessions::Unavailable(Unavailable::HomeUnknown));
    assert_eq!(
        branch(find_entry(&unknown.entries, "app"), "old").verdict,
        cleanup(None)
    );
}

#[test]
fn unavailable_detection_holds_every_action() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    ahead_branch(&ws, &app, "loose");
    // behind in a dirty checkout: the dirt names the hold
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    let feat = ws.dir("app-feat");
    ws.add_worktree(&app, &feat, &["feat"]);
    support::write(&feat, "notes.txt", "x\n");
    ws.assert_porcelain(&feat, &["?? notes.txt"]);
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.assert_track(&app, "feat", "[behind 1]");

    let reason = Unavailable::Unparseable {
        path: "/x/sessions/1.json".into(),
        error: "expected value".into(),
    };
    let run = ws.status_live(&LiveSessions::Unavailable(reason.clone()));
    assert_eq!(run.sessions, Sessions::Unavailable { reason });
    let e = find_entry(&run.entries, "app");
    assert!(e.checkouts.iter().all(|c| c.busy.is_empty()));
    assert_eq!(
        branch(e, "main").verdict,
        held(push(1), HeldBy::BusyUnknown)
    );
    // checked out nowhere, held all the same
    assert_eq!(
        branch(e, "loose").verdict,
        held(push(1), HeldBy::BusyUnknown)
    );
    assert_eq!(
        branch(e, "feat").verdict,
        held(ff(1), HeldBy::DirtyCheckout)
    );
}

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

#[test]
fn repos_status_reads_the_config_dirs_it_is_pointed_at() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    let claude = claude_dir(&ws, "claude");
    // each where its session file says, as a session's process is unless
    // it's moved
    let other = LiveChild::spawn_in(&app);
    let at_root = LiveChild::spawn_in(&ws.root());
    // the caller: this test process, which spawns the binary
    let me = std::process::id();
    claude.session(other.pid(), &other.proc_start(), &app);
    claude.session(at_root.pid(), &at_root.proc_start(), &ws.root());
    claude.session(me, &proc_start(me), &app);
    let repos = |config_dir: Option<&Path>, claude_pid: u32, args: &[&str]| {
        let mut cmd = ws.command(REPOS, &ws.root());
        if let Some(dir) = config_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        let out = cmd
            .env("CLAUDE_PID", claude_pid.to_string())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let line = |text: &str, label: &str| {
        text.lines()
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("no {label} line in:\n{text}"))
            .to_owned()
    };
    // this process isn't where its session file says: its label goes on
    let busy_in_app = |pid: u32| format!("pid {pid} ({}", path(&app));

    // the busy checkout holds by default; the unscoped session shows only
    // under --verbose; the caller shows nowhere
    let text = repos(Some(&claude.0), me, &["status"]);
    assert_eq!(line(&text, "held"), "held          push app +1 (busy)");
    assert!(!text.contains("unscoped"), "{text}");
    let verbose = repos(Some(&claude.0), me, &["status", "--verbose"]);
    assert_eq!(
        line(&verbose, "unscoped"),
        format!("unscoped      pid {} ({})", at_root.pid(), path(&ws.root()))
    );
    assert!(
        verbose.contains(&format!("busy: {}", busy_in_app(other.pid()))),
        "{verbose}"
    );
    assert!(!verbose.contains(&format!("pid {me} ")), "{verbose}");
    let json: serde_json::Value =
        serde_json::from_str(&repos(Some(&claude.0), me, &["status", "--json"])).unwrap();
    assert_eq!(
        json["sessions"],
        serde_json::json!({"kind": "available", "unscoped": [
            {"pid": at_root.pid(), "cwd": path(&ws.root()), "source": "session_file"},
        ]})
    );
    assert_eq!(
        json["entries"][0]["checkouts"][0]["busy"],
        serde_json::json!([{"pid": other.pid(), "cwd": path(&app), "source": "session_file"}])
    );

    // `CLAUDE_PID` naming a live session that isn't the binary's ancestor
    // excludes nothing: not that session, nor the real caller
    let verbose = repos(Some(&claude.0), other.pid(), &["status", "--verbose"]);
    for pid in [other.pid(), me] {
        assert!(verbose.contains(&busy_in_app(pid)), "{pid}: {verbose}");
    }

    // unset, or empty (as Claude Code reads it, `CLAUDE_CONFIG_DIR ||
    // ~/.claude`), it reads `$HOME/.claude` alone: the fixture's home has
    // none
    let home_claude = ClaudeDir::new(ws.base().join("home/.claude"));
    assert!(!home_claude.0.join("sessions").exists());
    let unset = [None, Some(Path::new(""))];
    for config_dir in unset {
        let text = repos(config_dir, me, &["status"]);
        assert_eq!(
            line(&text, "sync would"),
            "sync would    push app +1",
            "{config_dir:?}"
        );
    }
    // and set, `$HOME/.claude` too: a session under either is live
    let in_home = LiveChild::spawn_in(&app);
    home_claude.session(in_home.pid(), &in_home.proc_start(), &app);
    for config_dir in unset {
        let text = repos(config_dir, me, &["status"]);
        assert_eq!(
            line(&text, "held"),
            "held          push app +1 (busy)",
            "{config_dir:?}"
        );
    }
    let verbose = repos(Some(&claude.0), me, &["status", "--verbose"]);
    for pid in [other.pid(), in_home.pid()] {
        assert!(verbose.contains(&busy_in_app(pid)), "{pid}: {verbose}");
    }

    // a relative config dir: the binary's cwd resolves it, not the
    // sessions', so it vouches for nothing — though from here it names the
    // fixture's
    assert_eq!(
        ws.root().join("../claude").canonicalize().unwrap(),
        claude.0
    );
    let relative = ws
        .command(REPOS, &ws.root())
        .env("CLAUDE_CONFIG_DIR", "../claude")
        .env("CLAUDE_PID", me.to_string())
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(relative.status.code(), Some(0), "{relative:?}");
    let text = String::from_utf8(relative.stdout).unwrap();
    assert!(
        line(&text, "failed").starts_with(
            "failed        busy detection (config dir ../claude isn't an absolute path; "
        ),
        "{text}"
    );
    assert_eq!(
        line(&text, "held"),
        "held          push app +1 (busy unknown)"
    );

    // no HOME, or an empty one: Claude Code would fall back to the passwd
    // home, so `CLAUDE_CONFIG_DIR` alone vouches for nothing
    for home in [None, Some("")] {
        let mut cmd = ws.command(REPOS, &ws.root());
        match home {
            Some(home) => cmd.env("HOME", home),
            None => cmd.env_remove("HOME"),
        };
        let out = cmd
            .env("CLAUDE_CONFIG_DIR", &claude.0)
            .env("CLAUDE_PID", me.to_string())
            .args(["status", "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            json["sessions"],
            serde_json::json!({"kind": "unavailable", "reason": {"kind": "home_unknown"}}),
            "{home:?}"
        );
        assert_eq!(
            json["entries"][0]["branches"][0]["verdict"]["by"],
            "busy_unknown"
        );
    }

    // a file it can't vouch for: said by default, and everything held
    let file = claude.write_raw(&format!("{}.json", at_root.pid()), "{not json");
    let text = repos(Some(&claude.0), me, &["status"]);
    assert!(
        line(&text, "failed").starts_with(&format!(
            "failed        busy detection (can't parse {}: ",
            path(&file)
        )),
        "{text}"
    );
    assert!(text.contains("every push, ff, and move held)"), "{text}");
    assert_eq!(
        line(&text, "held"),
        "held          push app +1 (busy unknown)"
    );
}

/// `app`, and `lib` with `feat` ahead 1 checked out in an agent worktree
/// where Claude Code puts one, `lib/.claude/worktrees/agent-a1`; returns
/// `app`, `lib`, and the worktree.
fn lib_with_an_agent_worktree(ws: &mut FixtureWorkspace) -> (PathBuf, PathBuf, PathBuf) {
    let app = app(ws);
    let lib = ws.owned_repo("lib", &[]);
    support::write(&lib, ".git/info/exclude", ".claude/\n");
    ahead_branch(ws, &lib, "feat");
    let wt = lib.join(".claude/worktrees/agent-a1");
    ws.add_worktree(&lib, &wt, &["feat"]);
    (app, lib, wt)
}

/// Locks `repo`'s worktree `wt` with `reason`, as `git worktree lock`
/// writes it, in place of any lock it had.
fn relock(ws: &FixtureWorkspace, repo: &Path, wt: &Path, reason: &str) {
    let wt = wt.to_str().unwrap();
    // fails when it isn't locked, which is as good
    let _ = ws.git_output(repo, &["worktree", "unlock", wt]);
    ws.git(repo, &["worktree", "lock", "--reason", reason, wt]);
}

/// Under `live`, `lib`'s checkout at `wt` is busy with `holder` alone and
/// `feat`'s push held — or, without a holder, nothing in `lib` is busy and
/// the push acts.
fn assert_lib_feat(
    ws: &FixtureWorkspace,
    live: &LiveSessions,
    wt: &Path,
    holder: Option<&Session>,
) {
    let run = ws.status_live(live);
    let e = find_entry(&run.entries, "lib");
    let busy: Vec<(&str, &[Session])> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.busy.as_slice()))
        .chain(
            e.unprobed_worktrees
                .iter()
                .map(|u| (u.worktree.path.as_str(), u.busy.as_slice())),
        )
        .filter(|(_, busy)| !busy.is_empty())
        .collect();
    if let Some(s) = holder {
        assert_eq!(busy, [(path(wt).as_str(), std::slice::from_ref(s))]);
        assert_eq!(branch(e, "feat").verdict, held(push(1), HeldBy::Busy));
    } else {
        assert_eq!(busy, []);
        assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(1) });
    }
}

#[test]
fn a_worktree_claude_code_locked_is_busy_with_the_live_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    let (app, lib, wt) = lib_with_an_agent_worktree(&mut ws);
    let claude = claude_dir(&ws, "claude");
    // launched in `app`, its agent worktree in `lib`, which it `cd`'d into
    // in the Bash tool: no place of it is in `lib`
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    claude.session(pid, &start, &app);
    let s = child_session(&agent, &app, SessionSource::SessionFile);
    let live = read(&claude);
    assert_eq!(live, LiveSessions::Known(vec![s.clone()]));
    // unlocked: nothing places it in `lib`
    assert_lib_feat(&ws, &live, &wt, None);

    // as Claude Code writes it, with its start time or without, for a
    // subagent's worktree or one a session entered
    for reason in [
        format!("claude agent agent-a1 (pid {pid} start {start})"),
        format!("claude agent agent-a1 (pid {pid})"),
        format!("claude session feat (pid {pid} start {start})"),
        format!("claude agent a (pid 1) b (pid {pid} start {start})"),
    ] {
        relock(&ws, &lib, &wt, &reason);
        assert_lib_feat(&ws, &live, &wt, Some(&s));
    }

    let dead = dead_pid();
    let reused = (start.parse::<u64>().unwrap() + 1).to_string();
    // a live process with no session: whatever it is, the reader never
    // vouched for it
    let unrecorded = LiveChild::spawn();
    let (other, other_start) = (unrecorded.pid(), unrecorded.proc_start());
    for reason in [
        // its process exited
        format!("claude agent agent-a1 (pid {dead} start {start})"),
        format!("claude agent agent-a1 (pid {dead})"),
        // left by a process whose pid is the session's now
        format!("claude agent agent-a1 (pid {pid} start {reused})"),
        format!("claude agent agent-a1 (pid {pid} start 0{start})"),
        format!("claude agent agent-a1 (pid {other} start {other_start})"),
        // not Claude Code's
        format!("claude worker agent-a1 (pid {pid} start {start})"),
        format!("Claude agent agent-a1 (pid {pid} start {start})"),
        format!("agent agent-a1 (pid {pid} start {start})"),
        format!("claude agent  (pid {pid} start {start})"),
        format!("claude agent agent-a1 (pid {pid} start )"),
        format!("claude agent agent-a1 (pid {pid} start {start}"),
        format!("claude agent agent-a1 (pid {pid} start {start}) by hand"),
        format!("claude agent agent-a1 (pid  {pid})"),
        format!("claude agent agent-a1 (pid {pid}0000000000)"),
        format!("claude agent agent-a1 (pid {pid}, start {start})"),
        "on a removable drive".to_owned(),
    ] {
        relock(&ws, &lib, &wt, &reason);
        assert_lib_feat(&ws, &live, &wt, None);
    }

    // the caller's own lock: its worktree is its own to act on
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {start})"),
    );
    let as_caller = read_as(&claude, &agent, true);
    assert_eq!(as_caller, LiveSessions::Known(vec![]));
    assert_lib_feat(&ws, &as_caller, &wt, None);
    // unless the process tree doesn't back the claim
    assert_lib_feat(&ws, &read_as(&claude, &agent, false), &wt, Some(&s));
}

#[test]
fn a_missing_worktree_claude_code_locked_is_busy_with_the_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    let (app, lib, wt) = lib_with_an_agent_worktree(&mut ws);
    let claude = claude_dir(&ws, "claude");
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    claude.session(pid, &start, &app);
    let s = child_session(&agent, &app, SessionSource::SessionFile);
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {start})"),
    );
    // its files gone, git keeps it for the lock
    std::fs::remove_dir_all(&wt).unwrap();
    let live = read(&claude);
    let run = ws.status_live(&live);
    let e = find_entry(&run.entries, "lib");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    assert_eq!(e.unprobed_worktrees[0].worktree.why, UnprobedWhy::Missing);
    assert_lib_feat(&ws, &live, &wt, Some(&s));
    // a stale lock, its start another process's
    let reused = (start.parse::<u64>().unwrap() + 1).to_string();
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {reused})"),
    );
    assert_lib_feat(&ws, &live, &wt, None);
}

#[test]
fn a_primary_claude_code_locked_is_busy_with_the_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_repo("lib", "lib", "");
    let main_wt = ws.clone_owned("lib-main", "lib", &[]);
    ahead_branch(&ws, &main_wt, "feat");
    // the registry's dir is a linked worktree of the clone beside it
    let lib = ws.dir("lib");
    ws.add_worktree(&main_wt, &lib, &["feat"]);
    let claude = claude_dir(&ws, "claude");
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    // at the workspace root: in no checkout
    claude.session(pid, &start, &ws.root());
    let s = child_session(&agent, &ws.root(), SessionSource::SessionFile);
    let live = read(&claude);
    assert_lib_feat(&ws, &live, &lib, None);
    relock(
        &ws,
        &main_wt,
        &lib,
        &format!("claude session feat (pid {pid} start {start})"),
    );
    let run = ws.status_live(&live);
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "lib");
    assert!(e.checkouts[0].primary && e.checkouts[0].locked);
    assert_lib_feat(&ws, &live, &lib, Some(&s));
}
