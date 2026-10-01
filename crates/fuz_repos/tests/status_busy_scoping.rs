//! The scoping of live sessions to checkouts: which checkout or worktree a
//! session's cwd or process places it in.

mod support;

use std::path::Path;

use fuz_repos::report::{EntryStatus, Sessions};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchHold, Verdict};
use support::busy::{
    ahead_branch, app, app_with_a_feat_worktree, by_pid, claude_dir, held, live_session,
    move_by_hand, push, read, session,
};
use support::{FixtureWorkspace, LiveChild, branch, ff, find_entry, path, roster_worker};

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
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
    assert_eq!(branch(e, "feat").verdict, held(ff(1), BranchHold::Busy));
    assert_eq!(branch(e, "side").verdict, held(push(1), BranchHold::Busy));
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
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
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
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
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
    assert_eq!(branch(e, "hollow").verdict, held(push(1), BranchHold::Busy));
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
        assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
        assert_eq!(branch(e, "feat").verdict, held(push(2), BranchHold::Busy));
        assert_eq!(branch(e, "hollow").verdict, held(push(1), BranchHold::Busy));
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
                held(push(1), BranchHold::Busy)
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
    // the exclude, in the separate git dir, keeps the primary clean
    for c in [&app, &in_primary, &wt, &in_git_dir] {
        ws.assert_clean(c);
    }

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
    ws.assert_clean(&moved);

    let s = session(9, &moved, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(busy_checkouts(e), paths(&[&wt, &q]));
    assert_eq!(branch(e, "feat").verdict, held(push(2), BranchHold::Busy));
    assert_eq!(branch(e, "gamma").verdict, held(push(1), BranchHold::Busy));
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
    assert_eq!(branch(e, "feat").verdict, held(push(1), BranchHold::Busy));
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
    assert_eq!(branch(e, "feat").verdict, held(push(1), BranchHold::Busy));
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
