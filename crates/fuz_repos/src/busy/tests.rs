use super::*;
use crate::sessions::SessionSource;

/// A checkout by path alone, with no git dir and no lock.
fn keys(path: &str) -> CheckoutKeys {
    CheckoutKeys {
        path: path.to_owned(),
        ..CheckoutKeys::default()
    }
}

/// Entries' checkouts by path alone, with no git dirs.
fn at_paths(entries: Vec<Vec<String>>) -> Vec<EntryCheckouts> {
    entries
        .into_iter()
        .map(|paths| EntryCheckouts {
            checkouts: paths.iter().map(|p| keys(p)).collect(),
            ..EntryCheckouts::default()
        })
        .collect()
}

#[test]
fn scoping_fails_closed_on_a_cwd_it_cannot_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    std::fs::create_dir(base.join("app")).unwrap();
    std::os::unix::fs::symlink(base.join("loop"), base.join("loop")).unwrap();
    let s = |cwd: &Path| {
        Session::at(
            1,
            0,
            cwd.to_string_lossy().into_owned(),
            SessionSource::SessionFile,
        )
    };
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let checkouts = at_paths(vec![vec![path(&base.join("app"))]]);
    let live = LiveSessions::Known(vec![s(&base.join("app")), s(&base.join("loop/x"))]);
    let (report, per_entry) = scope_sessions(&live, &checkouts);
    assert_eq!(
        report,
        Sessions::Unavailable {
            reason: Unavailable::Unreadable {
                path: path(&base.join("loop")),
                error: "Too many levels of symbolic links (os error 40)".into(),
            }
        }
    );
    assert_eq!(per_entry, [EntrySessions::unavailable()]);
}

#[test]
fn a_checkout_it_cannot_resolve_is_unresolved_for_each_entry_it_is_of() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for dir in ["app", "lib", "shared"] {
        std::fs::create_dir(base.join(dir)).unwrap();
    }
    std::os::unix::fs::symlink(base.join("loop"), base.join("loop")).unwrap();
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let s = |pid, cwd: &Path| Session::at(pid, 0, path(cwd), SessionSource::SessionFile);
    // `app`'s worktree in the loop, beside a checkout it shares with
    // `shared`, whose own worktree is the same loop
    let looped = path(&base.join("loop/wt"));
    let checkouts = at_paths(vec![
        vec![path(&base.join("app")), looped.clone()],
        vec![path(&base.join("lib"))],
        vec![path(&base.join("shared")), looped.clone()],
    ]);
    let unresolved = BTreeMap::from([(
        looped,
        UnresolvedCheckout {
            path: path(&base.join("loop")),
            error: "Too many levels of symbolic links (os error 40)".into(),
        },
    )]);
    let held = |per_entry: &[EntrySessions]| {
        assert_eq!(per_entry[0].unresolved, unresolved);
        assert!(per_entry[1].unresolved.is_empty());
        assert_eq!(per_entry[2].unresolved, unresolved);
    };
    // with no session live, and with some: the same checkouts held
    let (report, per_entry) = scope_sessions(&LiveSessions::Known(vec![]), &checkouts);
    assert_eq!(report, Sessions::Available { unscoped: vec![] });
    held(&per_entry);
    assert!(
        per_entry
            .iter()
            .all(|e| e.detection == Detection::Available)
    );
    let in_lib = s(2, &base.join("lib/src"));
    let at_base = s(3, &base);
    let live = LiveSessions::Known(vec![in_lib.clone(), at_base.clone()]);
    let (report, per_entry) = scope_sessions(&live, &checkouts);
    assert_eq!(
        report,
        Sessions::Available {
            unscoped: vec![at_base]
        }
    );
    held(&per_entry);
    assert_eq!(per_entry[1].at(&path(&base.join("lib"))), [in_lib]);
    assert!(per_entry[0].busy.is_empty() && per_entry[2].busy.is_empty());
    // and with detection unavailable
    let (_, per_entry) = scope_sessions(
        &LiveSessions::Unavailable(Unavailable::HomeUnknown),
        &checkouts,
    );
    held(&per_entry);
}

#[test]
fn a_shared_common_dir_is_its_commondir_or_where_its_refs_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let git_dir = base.join("hand");
    let common = base.join("common");
    std::fs::create_dir_all(git_dir.join("refs")).unwrap();
    std::fs::create_dir(&common).unwrap();
    let any = |_: &Path| true;
    let shared = |content: &[u8]| {
        std::fs::write(git_dir.join("commondir"), content).unwrap();
        shared_common_dir(&git_dir, any)
    };
    // a `commondir`, read as git reads it (`gitdir::read_commondir`),
    // resolved
    assert_eq!(shared(b"../common\n"), Some(common.clone()));
    let absolute = format!("{}\n", common.display());
    assert_eq!(shared(absolute.as_bytes()), Some(common.clone()));
    assert_eq!(shared(b"\n"), Some(git_dir.clone()));
    // one git can't use stops git, and no `refs` is looked at
    assert_eq!(shared(b""), None);
    assert_eq!(shared(b"../nowhere\n"), None);
    assert_eq!(shared(b"../common \n"), None);
    std::fs::remove_file(git_dir.join("commondir")).unwrap();
    std::os::unix::fs::symlink("nowhere", git_dir.join("commondir")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, any), None);
    // without one, the dir its `refs` resolves in
    std::fs::remove_file(git_dir.join("commondir")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, any), Some(git_dir.clone()));
    std::fs::remove_dir(git_dir.join("refs")).unwrap();
    std::fs::create_dir_all(common.join("refs/heads")).unwrap();
    std::os::unix::fs::symlink("../common/refs", git_dir.join("refs")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, any), Some(common.clone()));
    // a `refs` of another name is no git dir's
    std::fs::remove_file(git_dir.join("refs")).unwrap();
    std::fs::create_dir(common.join("other")).unwrap();
    std::os::unix::fs::symlink("../common/other", git_dir.join("refs")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, any), None);
    // or the dir `refs/heads` resolves in, when only `heads` is linked
    // and `refs` names no common dir known
    std::fs::remove_file(git_dir.join("refs")).unwrap();
    std::fs::create_dir(git_dir.join("refs")).unwrap();
    std::os::unix::fs::symlink("../../common/refs/heads", git_dir.join("refs/heads")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, any), Some(git_dir.clone()));
    let known = |dir: &Path| dir == common;
    assert_eq!(shared_common_dir(&git_dir, known), Some(common.clone()));
    let unknown = |_: &Path| false;
    assert_eq!(shared_common_dir(&git_dir, unknown), None);
    // a `heads` of another name, or under no `refs`, is no git dir's
    std::fs::remove_file(git_dir.join("refs/heads")).unwrap();
    std::os::unix::fs::symlink("../../common/other", git_dir.join("refs/heads")).unwrap();
    assert_eq!(shared_common_dir(&git_dir, known), None);
}

#[test]
fn the_deepest_containing_checkout_by_component() {
    let c: Vec<PathBuf> = [
        "/ws/app",
        "/ws/app/.claude/worktrees/feat",
        "/ws/app-wt",
        "/ws/b",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    let at = |cwd: &str| deepest_containing(Path::new(cwd), c.iter().map(PathBuf::as_path));
    assert_eq!(at("/ws/app"), [0]);
    assert_eq!(at("/ws/app/src"), [0]);
    assert_eq!(at("/ws/app/.claude/worktrees/feat/src"), [1]);
    assert_eq!(at("/ws/app/.claude/worktrees/feature"), [0]);
    assert_eq!(at("/ws/app-wt/x"), [2]);
    assert!(at("/ws").is_empty());
    assert!(at("/elsewhere").is_empty());
    // one path that's two entries' checkouts: both
    let tie: Vec<PathBuf> = ["/ws/a", "/ws/b", "/ws/b"]
        .iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(
        deepest_containing(Path::new("/ws/b/x"), tie.iter().map(PathBuf::as_path)),
        [1, 2]
    );
}

#[test]
fn scoping_marks_checkouts_and_leaves_the_rest_unscoped() {
    let s = |pid, cwd: &str| Session::at(pid, 0, cwd.into(), SessionSource::SessionFile);
    let live = LiveSessions::Known(vec![
        s(1, "/nonexistent-ws/app/src"),
        s(2, "/nonexistent-ws"),
        s(3, "/nonexistent-ws/b"),
    ]);
    let checkouts = at_paths(vec![
        vec!["/nonexistent-ws/app".to_owned()],
        vec!["/nonexistent-ws/b".to_owned()],
    ]);
    let (report, per_entry) = scope_sessions(&live, &checkouts);
    assert_eq!(
        report,
        Sessions::Available {
            unscoped: vec![s(2, "/nonexistent-ws")]
        }
    );
    assert_eq!(
        per_entry[0].at("/nonexistent-ws/app"),
        [s(1, "/nonexistent-ws/app/src")]
    );
    assert_eq!(
        per_entry[1].at("/nonexistent-ws/b"),
        [s(3, "/nonexistent-ws/b")]
    );
    assert_eq!(per_entry[0].detection, Detection::Available);

    let reason = Unavailable::HomeUnknown;
    let (report, per_entry) =
        scope_sessions(&LiveSessions::Unavailable(reason.clone()), &checkouts);
    assert_eq!(report, Sessions::Unavailable { reason });
    assert!(
        per_entry
            .iter()
            .all(|e| e.detection == Detection::Unavailable)
    );
}

#[test]
fn scoping_marks_the_checkouts_whose_lock_names_a_session() {
    let s = |pid, start| {
        Session::at(
            pid,
            start,
            "/nonexistent-ws".into(),
            SessionSource::SessionFile,
        )
    };
    let live = LiveSessions::Known(vec![s(1, 10), s(2, 20)]);
    let checkout = |paths: &[&str], locks: &[(&str, &str)]| EntryCheckouts {
        checkouts: paths
            .iter()
            .map(|&p| CheckoutKeys {
                lock: locks
                    .iter()
                    .find(|&&(l, _)| l == p)
                    .map(|&(_, r)| r.to_owned()),
                ..keys(p)
            })
            .collect(),
        ..EntryCheckouts::default()
    };
    let checkouts = vec![
        checkout(
            &[
                "/nonexistent-ws/app",
                "/nonexistent-ws/app-a",
                "/nonexistent-ws/app-b",
            ],
            &[
                ("/nonexistent-ws/app-a", "claude agent a (pid 1 start 10)"),
                ("/nonexistent-ws/app-b", "claude agent b (pid 2 start 21)"),
            ],
        ),
        // a primary locked for a session, and a lock by hand
        checkout(
            &["/nonexistent-ws/lib", "/nonexistent-ws/lib-c"],
            &[
                ("/nonexistent-ws/lib", "claude session lib (pid 2)"),
                ("/nonexistent-ws/lib-c", "pid 1"),
            ],
        ),
    ];
    let (report, per_entry) = scope_sessions(&live, &checkouts);
    assert_eq!(report, Sessions::Available { unscoped: vec![] });
    let busy = |e: &EntrySessions| {
        e.busy
            .iter()
            .map(|(path, sessions)| {
                let pids: Vec<u32> = sessions.iter().map(|s| s.pid).collect();
                (path.clone(), pids)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        busy(&per_entry[0]),
        [("/nonexistent-ws/app-a".to_owned(), vec![1])]
    );
    assert_eq!(
        busy(&per_entry[1]),
        [("/nonexistent-ws/lib".to_owned(), vec![2])]
    );
}

#[test]
fn working_is_busy_less_the_agent_worktrees_a_session_elsewhere_holds() {
    let s = |pid, start, cwd: &str| Session::at(pid, start, cwd.into(), SessionSource::SessionFile);
    let app = "/nonexistent-ws/app";
    let wt1 = "/nonexistent-ws/app/.claude/worktrees/wt1";
    let wt2 = "/nonexistent-ws/app/.claude/worktrees/wt2";
    let live = LiveSessions::Known(vec![
        // in the primary
        s(1, 10, app),
        // in one agent worktree
        s(2, 20, &format!("{wt1}/src")),
        // at the root, with the other agent worktree locked for it
        s(3, 30, "/nonexistent-ws"),
    ]);
    let checkouts = vec![EntryCheckouts {
        checkouts: vec![
            keys(app),
            keys(wt1),
            CheckoutKeys {
                lock: Some("claude agent wt2 (pid 3 start 30)".to_owned()),
                ..keys(wt2)
            },
        ],
        common_dir: Some(PathBuf::from(format!("{app}/.git"))),
    }];
    let (report, per_entry) = scope_sessions(&live, &checkouts);
    assert_eq!(report, Sessions::Available { unscoped: vec![] });
    let pids = |sessions: &[Session]| sessions.iter().map(|s| s.pid).collect::<Vec<_>>();
    let e = &per_entry[0];
    // busy: every session in the repo holds both agent worktrees
    assert_eq!(pids(e.at(app)), [1]);
    assert_eq!(pids(e.at(wt1)), [1, 2]);
    assert_eq!(pids(e.at(wt2)), [1, 2, 3]);
    // working: each where it is, or where its lock is
    assert_eq!(pids(e.working_at(app)), [1]);
    assert_eq!(pids(e.working_at(wt1)), [2]);
    assert_eq!(pids(e.working_at(wt2)), [3]);
}
