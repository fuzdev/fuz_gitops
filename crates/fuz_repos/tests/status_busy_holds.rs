//! The holds live sessions put on sync's actions, and when detection is
//! unavailable.

mod support;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::Sessions;
use fuz_repos::sessions::{LiveSessions, SessionSource, Unavailable};
use fuz_repos::state::{CleanupReason, HeldBy, Verdict};
use fuz_repos::status::StatusRun;
use support::busy::{ahead_branch, app, held, push, session};
use support::{FixtureWorkspace, branch, ff, find_entry, path};

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
