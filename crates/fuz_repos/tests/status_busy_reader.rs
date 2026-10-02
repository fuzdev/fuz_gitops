//! The sessions reader over fixture Claude Code config dirs whose files name
//! real live pids (the test's own children), and the binary reading the config
//! dirs it is pointed at.

mod support;

use std::path::{Path, PathBuf};

use fuz_repos::sessions::{LiveSessions, SessionSource, Unavailable, read_live_sessions};
use support::busy::{app, by_pid, child_session, claude_dir, read, read_as, source};
use support::cli::REPOS;
use support::{
    ClaudeDir, FixtureWorkspace, LiveChild, dead_pid, own_pid_domain, path, proc_start,
    roster_worker,
};

/// A session file for `pid` as Claude Code writes it.
fn session_doc(pid: u32, proc_start: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "pid": pid, "procStart": proc_start, "cwd": path(cwd), "pidDomain": own_pid_domain(),
    })
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
            {
                "pid": at_root.pid(),
                "cwd": path(&ws.root()),
                "worktree": null,
                "process_cwd": null,
                "source": "session_file"
            },
        ]})
    );
    assert_eq!(
        json["entries"][0]["checkouts"][0]["busy"],
        serde_json::json!([{
            "pid": other.pid(),
            "cwd": path(&app),
            "worktree": null,
            "process_cwd": null,
            "source": "session_file"
        }])
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
    assert!(
        text.contains("every push, ff, move, and rebase held)"),
        "{text}"
    );
    assert_eq!(
        line(&text, "held"),
        "held          push app +1 (busy unknown)"
    );
}
