//! `repos push` over fixture workspaces through the library: the branch checked
//! out at each target pushed as a fast-forward, through sync's own push, the
//! exact commit classified, nothing else, and every way it is held or left
//! alone. Each run is followed by the exact refs it should leave on both sides.
//!
//! Pushes reach the local bare remotes over the fixture's own `ssh`, which
//! serves the registry's SSH URLs (the support module says how).

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::report::{BranchSyncHold, FetchOutcome, NoUpstreamWhy, PushOutcome};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchNeedsHuman, Relation, SyncAction, Verdict};
use support::cli::{repos, stderr, stdout};
use support::push::{ahead, feat_ahead, only, pushed, pushes_served, remote_refs, with};
use support::{FixtureWorkspace, LiveChild, branch, find_entry, quiet};

// --- what's pushed ---

#[test]
fn pushes_the_branch_checked_out_and_nothing_else() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "side");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "side", "origin/side"]);
    let tip = ws.commit(&app, "local");
    // another branch ahead, checked out nowhere: sync's to push, not this
    let tree = format!("{}^{{tree}}", "refs/heads/side");
    let side = ws.git(&app, &["commit-tree", &tree, "-p", "side", "-m", "side"]);
    ws.git(&app, &["update-ref", "refs/heads/side", &side]);
    // a tag on the commit, and a config that would follow it
    ws.git(&app, &["tag", "v1", &tip]);
    ws.git(&app, &["config", "push.followTags", "true"]);
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.assert_track(&app, "side", "[ahead 1]");
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");
    let origin_main = remote_before["refs/heads/main"].clone();
    let local_before = ws.refs(&app);

    let run = ws.push(&["app"]);

    assert_eq!(only(&run), (Some("main"), &pushed(&origin_main, &tip)));
    assert!(run.pushes.iter().all(|p| p.outcome.in_sync()));
    // the one ref, no tag, the other branch where it was
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", &tip)])
    );
    // the remote-tracking ref moved to the pushed commit, no refetch (and
    // `origin/HEAD`, which names it); the rest as they were
    assert_eq!(
        ws.refs(&app),
        with(
            &local_before,
            &[
                ("refs/remotes/origin/main", &tip),
                ("refs/remotes/origin/HEAD", &tip)
            ]
        )
    );
    assert_eq!(
        ws.git(
            &app,
            &["reflog", "-1", "--format=%gs", "refs/remotes/origin/main"]
        ),
        "repos: update by push"
    );
    ws.assert_track(&app, "main", "");
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/app'"]);
    // and in sync once there: a rerun has nothing to push
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("main"), &PushOutcome::InSync));
    assert_eq!(pushes_served(&ws).len(), 1);
}

#[test]
fn targets_name_their_checkouts() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    // the dir isn't the key: both are targets
    ws.declare_repo("app", "app", "dir = \"app-dir\"");
    let app = ws.clone_owned("app-dir", "app", &[]);
    let main_tip = ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    // a linked worktree outside the workspace, on `feat`, ahead too
    let wt = ws.outside("feat-wt");
    ws.add_worktree(&app, &wt, &["feat"]);
    ws.assert_head(&wt, Some("feat"));
    ws.assert_upstream(&app, "feat", "refs/remotes/origin/feat");
    let feat_tip = ws.commit(&wt, "feat-local");
    ws.assert_track(&app, "feat", "[ahead 1]");
    ws.write_registry();
    std::fs::create_dir(app.join("sub")).unwrap();
    let remote_before = remote_refs(&ws, "app");
    let (origin_main, origin_feat) = (
        remote_before["refs/heads/main"].clone(),
        remote_before["refs/heads/feat"].clone(),
    );

    // the cwd in the worktree, no target: its branch, not the primary's
    let run = ws.push_from(&[], &wt);
    assert_eq!(only(&run), (Some("feat"), &pushed(&origin_feat, &feat_tip)));
    assert_eq!(run.pushes[0].checkout, wt.to_str().unwrap());
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/feat", &feat_tip)])
    );
    // a key, a dir name, and a path in the primary, the cwd a subdir: each
    // names the primary, pushed once and in sync after
    let run = ws.push_from(&["app", "app-dir", "."], &app.join("sub"));
    assert_eq!(only(&run), (Some("main"), &pushed(&origin_main, &main_tip)));
    assert_eq!(run.pushes[0].checkout, app.to_str().unwrap());
    let run = ws.push_from(&[], &app.join("sub"));
    assert_eq!(only(&run), (Some("main"), &PushOutcome::InSync));
    // the worktree by its path, beside the primary by key
    let run = ws.push_from(&["app", wt.to_str().unwrap()], &ws.root());
    let got: Vec<_> = run
        .pushes
        .iter()
        .map(|p| (p.branch.as_deref(), &p.outcome))
        .collect();
    assert_eq!(
        got,
        [
            (Some("main"), &PushOutcome::InSync),
            (Some("feat"), &PushOutcome::InSync)
        ]
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(
            &remote_before,
            &[
                ("refs/heads/main", &main_tip),
                ("refs/heads/feat", &feat_tip)
            ]
        )
    );
    assert_eq!(pushes_served(&ws).len(), 2);
}

#[test]
fn a_dirty_tree_still_pushes() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    support::write(&app, "README", "edited\n");
    support::write(&app, "new.txt", "new\n");
    ws.assert_porcelain(&app, &[" M README", "?? new.txt"]);
    let origin_main = remote_refs(&ws, "app")["refs/heads/main"].clone();

    let run = ws.push(&["app"]);

    assert_eq!(only(&run), (Some("main"), &pushed(&origin_main, &tip)));
    // the files as they were: a push moves refs alone
    ws.assert_porcelain(&app, &[" M README", "?? new.txt"]);
}

// --- what isn't ---

#[test]
fn a_branch_behind_is_never_moved() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.write_registry();
    let local = ws.git(&app, &["rev-parse", "main"]);
    ws.upstream_commit("app", "main");
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);

    assert_eq!(only(&run), (Some("main"), &PushOutcome::NotAhead));
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").relation, Relation::Behind { commits: 1 });
    assert!(!run.pushes[0].outcome.in_sync());
    // fetched, never fast-forwarded: sync's to do
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
    ws.assert_track(&app, "main", "[behind 1]");
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// A diverged branch that isn't the registry's: never rebased (the
/// registry's own is, `push_rebase.rs`).
#[test]
fn a_diverged_feature_branch_is_a_persons() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = feat_ahead(&mut ws);
    ws.upstream_commit("app", "feat");
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);

    assert_eq!(
        only(&run),
        (
            Some("feat"),
            &PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Diverged
            }
        )
    );
    ws.assert_track(&app, "feat", "[ahead 1, behind 1]");
    assert_eq!(ws.git(&app, &["rev-parse", "feat"]), tip);
    ws.assert_clean(&app);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_detached_head_has_nothing_to_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    ws.git(&app, &["switch", "-q", "--detach", "HEAD"]);
    ws.assert_head(&app, None);
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);

    // `main` stays ahead: it isn't what's checked out
    assert_eq!(only(&run), (None, &PushOutcome::Detached));
    assert_eq!(remote_refs(&ws, "app"), remote_before);
}

#[test]
fn a_remote_branch_is_never_created_without_new_branch() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = feat_ahead(&mut ws);
    // no upstream at all
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    ws.commit(&app, "topic");
    ws.assert_upstream(&app, "topic", "");
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::NoUpstream {
                why: NoUpstreamWhy::Creatable
            }
        )
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);

    // an upstream deleted on origin: the fetch prunes it, and it stays gone
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.upstream_delete_branch("app", "feat");
    let remote_before = remote_refs(&ws, "app");
    assert!(!remote_before.contains_key("refs/heads/feat"));
    let run = ws.push(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("feat"),
            &PushOutcome::NoUpstream {
                why: NoUpstreamWhy::Creatable
            }
        )
    );
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "feat").relation, Relation::Gone);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_busy_checkout_holds_the_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        app.to_str().unwrap().to_owned(),
        SessionSource::SessionFile,
    )]);

    let run = ws.push_with(&["app"], &ws.root(), &|| live.clone());
    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::Held {
                by: BranchSyncHold::Busy
            }
        )
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);

    // one arriving after the fetch holds it too, re-read right before
    let calls = AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            live.clone()
        }
    };
    let run = ws.push_with(&["app"], &ws.root(), &read);
    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::Held {
                by: BranchSyncHold::Busy
            }
        )
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn origin_drift_holds_the_push() {
    // (config set in the clone, the hold its verdict names)
    type Drift = fn(&FixtureWorkspace, &Path);
    let cases: [(&str, Drift, BranchSyncHold); 2] = [
        (
            "origin",
            |ws, app| ws.set_origin(app, "other", "git@github.com:me/other"),
            BranchSyncHold::Entry,
        ),
        (
            "pushurl",
            |ws, app| {
                ws.git(
                    app,
                    &["config", "remote.origin.pushurl", "git@github.com:me/other"],
                );
            },
            BranchSyncHold::PushUrl,
        ),
    ];
    for (case, drift, by) in cases {
        let mut ws = FixtureWorkspace::new();
        let (app, _) = ahead(&mut ws);
        // another repo with the same history: a push sent there would land
        let from = format!("file://{}", ws.bare("app").display());
        let other = ws.bare("other");
        ws.git(
            ws.base(),
            &["clone", "-q", "--bare", &from, other.to_str().unwrap()],
        );
        drift(&ws, &app);
        // still ahead of what the drifted origin holds
        ws.git(&app, &["fetch", "-q", "origin"]);
        ws.assert_track(&app, "main", "[ahead 1]");
        let app_before = remote_refs(&ws, "app");
        let other_before = remote_refs(&ws, "other");

        let run = ws.push(&["app"]);

        assert_eq!(
            only(&run),
            (Some("main"), &PushOutcome::Held { by }),
            "{case}"
        );
        assert_eq!(remote_refs(&ws, "app"), app_before, "{case}");
        assert_eq!(remote_refs(&ws, "other"), other_before, "{case}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

/// A branch with no upstream reads so whatever the fetch: its config says
/// it, not origin's refs. A gone upstream is the fetch's to say, so a
/// failed fetch holds it — and a creation, which pushes.
#[test]
fn no_upstream_reads_so_when_the_fetch_fails() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    ws.commit(&app, "topic");
    ws.write_registry();
    std::fs::remove_dir_all(ws.bare("app")).unwrap();

    let run = ws.push(&["app"]);
    assert!(matches!(run.pushes[0].fetch, FetchOutcome::Failed { .. }));
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::NoUpstream {
                why: NoUpstreamWhy::Creatable
            }
        )
    );

    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::Held {
                by: BranchSyncHold::FetchFailed
            }
        )
    );
    ws.assert_upstream(&app, "topic", "");
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// `a`'s repo with its work tree pointed at `b`'s checkout
/// (`core.worktree`): from inside `a`, git's top level is `b`, whose files
/// are another entry's. No checkout of `a`'s to push, and never `b`'s.
#[test]
fn a_work_tree_elsewhere_names_no_checkout_to_push() {
    let mut ws = FixtureWorkspace::new();
    let a = ws.owned_repo("a", &[]);
    let b = ws.owned_repo("b", &[]);
    ws.commit(&b, "local");
    ws.assert_track(&b, "main", "[ahead 1]");
    ws.git(&a, &["config", "core.worktree", b.to_str().unwrap()]);
    assert_eq!(
        ws.git(&a, &["rev-parse", "--show-toplevel"]),
        b.to_str().unwrap()
    );
    ws.write_registry();
    let before = (remote_refs(&ws, "a"), remote_refs(&ws, "b"));

    let out = repos(&ws, &a, &["push"]);
    assert_eq!(out.status.code(), Some(2), "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).starts_with("error: ") && stderr(&out).contains("checkout"),
        "{}",
        stderr(&out)
    );
    assert_eq!((remote_refs(&ws, "a"), remote_refs(&ws, "b")), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    // from `b` itself it's `b`'s to push
    let out = repos(&ws, &b, &["push"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/b'"]);
}
