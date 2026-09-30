//! Each branch relation and verdict, from real repos in a fixture workspace.

mod support;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::state::{BranchNeedsHuman, CleanupReason, HeldBy, Relation, SyncAction, Verdict};
use support::{FixtureWorkspace, branch, branch_names, find_entry};

#[test]
fn default_branch_in_sync_ahead_behind_and_diverged() {
    let mut ws = FixtureWorkspace::new();
    for name in ["even", "ahead", "behind", "diverged"] {
        ws.remote(name, &[]);
        ws.declare_repo(name, name, "");
        ws.clone_owned(name, name, &[]);
    }
    let ahead = ws.dir("ahead");
    ws.commit(&ahead, "local-1");
    ws.commit(&ahead, "local-2");
    ws.upstream_commit("behind", "main");
    ws.git(&ws.dir("behind"), &["fetch", "-q", "origin"]);
    ws.upstream_commit("diverged", "main");
    let diverged = ws.dir("diverged");
    ws.commit(&diverged, "local");
    ws.git(&diverged, &["fetch", "-q", "origin"]);

    ws.assert_track(&ws.dir("even"), "main", "");
    ws.assert_upstream(&ws.dir("even"), "main", "refs/remotes/origin/main");
    ws.assert_track(&ahead, "main", "[ahead 2]");
    ws.assert_track(&ws.dir("behind"), "main", "[behind 1]");
    ws.assert_track(&diverged, "main", "[ahead 1, behind 1]");
    for name in ["even", "ahead", "behind", "diverged"] {
        ws.assert_clean(&ws.dir(name));
    }

    let entries = ws.status();
    for e in &entries {
        assert!(e.needs_human.is_empty(), "{}: {:?}", e.key, e.needs_human);
        assert_eq!(e.probe_error, None, "{}", e.key);
        assert_eq!(branch_names(e), ["main"], "{}", e.key);
    }

    let even = branch(find_entry(&entries, "even"), "main");
    assert_eq!(even.relation, Relation::InSync);
    assert_eq!(even.verdict, Verdict::Quiet);
    assert_eq!(even.upstream.as_deref(), Some("origin/main"));
    assert_eq!(even.unique_commits, 0);

    let ahead = branch(find_entry(&entries, "ahead"), "main");
    assert_eq!(ahead.relation, Relation::Ahead { commits: 2 });
    assert_eq!(ahead.unique_commits, 2);
    assert_eq!(
        ahead.verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 2 }
        }
    );

    // behind and checked out in a clean checkout: fast-forward in place
    let behind = branch(find_entry(&entries, "behind"), "main");
    assert_eq!(behind.relation, Relation::Behind { commits: 1 });
    assert_eq!(behind.unique_commits, 0);
    assert!(behind.worktree.is_some());
    assert_eq!(
        behind.verdict,
        Verdict::Act {
            action: SyncAction::FastForward { commits: 1 }
        }
    );

    let diverged = branch(find_entry(&entries, "diverged"), "main");
    assert_eq!(
        diverged.relation,
        Relation::Diverged {
            ahead: 1,
            behind: 1
        }
    );
    assert_eq!(diverged.unique_commits, 1);
    assert_eq!(
        diverged.verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
}

#[test]
fn archived_ahead_needs_a_human_and_archived_behind_still_fast_forwards() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("old", &[]);
    ws.declare_repo("old", "old", "archived = true");
    let old = ws.clone_owned("old", "old", &[]);
    // ahead on a feature branch, behind on main
    ws.upstream_commit("old", "feat");
    ws.git(&old, &["fetch", "-q", "origin"]);
    ws.git(&old, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.upstream_commit("old", "main");
    ws.git(&old, &["fetch", "-q", "origin"]);
    ws.git(&old, &["checkout", "-q", "feat"]);
    ws.commit(&old, "local");
    ws.git(&old, &["checkout", "-q", "main"]);
    ws.assert_track(&old, "main", "[behind 1]");
    ws.assert_track(&old, "feat", "[ahead 1]");
    ws.assert_clean(&old);

    let e = ws.entry("old");
    assert!(e.archived);
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::ArchivedAhead
        }
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Act {
            action: SyncAction::FastForward { commits: 1 }
        }
    );
}

#[test]
fn on_a_feature_branch_with_main_behind() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(&app, &["checkout", "-q", "--track", "origin/feat"]);
    ws.assert_head(&app, Some("feat"));
    ws.assert_track(&app, "feat", "");
    ws.assert_upstream(&app, "feat", "refs/remotes/origin/feat");
    ws.assert_track(&app, "main", "[behind 1]");
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let feat = branch(&e, "feat");
    assert_eq!(feat.relation, Relation::InSync);
    assert_eq!(feat.verdict, Verdict::Quiet);
    assert!(feat.worktree.is_some());
    // not checked out anywhere, so nothing holds the fast-forward
    let main = branch(&e, "main");
    assert_eq!(main.worktree, None);
    assert_eq!(main.relation, Relation::Behind { commits: 1 });
    assert_eq!(
        main.verdict,
        Verdict::Act {
            action: SyncAction::FastForward { commits: 1 }
        }
    );
}

#[test]
fn tracking_feature_branches_ahead_behind_and_diverged() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    for b in ["feat_a", "feat_b", "feat_c"] {
        ws.upstream_commit("app", b);
    }
    ws.git(&app, &["fetch", "-q", "origin"]);
    for b in ["feat_a", "feat_b", "feat_c"] {
        ws.git(
            &app,
            &["branch", "-q", "--track", b, &format!("origin/{b}")],
        );
    }
    for b in ["feat_a", "feat_c"] {
        ws.git(&app, &["checkout", "-q", b]);
        ws.commit(&app, &format!("local-{b}"));
    }
    ws.git(&app, &["checkout", "-q", "main"]);
    for b in ["feat_b", "feat_c"] {
        ws.upstream_commit("app", b);
    }
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "feat_a", "[ahead 1]");
    ws.assert_track(&app, "feat_b", "[behind 1]");
    ws.assert_track(&app, "feat_c", "[ahead 1, behind 1]");
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert_eq!(branch_names(&e), ["feat_a", "feat_b", "feat_c", "main"]);
    let a = branch(&e, "feat_a");
    assert_eq!(a.relation, Relation::Ahead { commits: 1 });
    assert_eq!(a.upstream.as_deref(), Some("origin/feat_a"));
    assert_eq!(
        a.verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
    let b = branch(&e, "feat_b");
    assert_eq!(b.relation, Relation::Behind { commits: 1 });
    assert_eq!(
        b.verdict,
        Verdict::Act {
            action: SyncAction::FastForward { commits: 1 }
        }
    );
    let c = branch(&e, "feat_c");
    assert_eq!(
        c.relation,
        Relation::Diverged {
            ahead: 1,
            behind: 1
        }
    );
    assert_eq!(
        c.verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(branch(&e, "main").verdict, Verdict::Quiet);
}

#[test]
fn a_local_only_branch_carries_its_count_and_age() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["checkout", "-q", "-b", "wip"]);
    ws.commit(&app, "wip-1");
    let tip = ws.commit(&app, "wip-2");
    let at = ws.committer_time(&app, &tip);
    ws.git(&app, &["checkout", "-q", "main"]);
    ws.assert_upstream(&app, "wip", "");
    ws.assert_count(&app, &["wip", "--not", "--remotes"], 2);
    ws.assert_clean(&app);
    assert!(at > support::CLOCK_START, "the fixture clock set the date");

    let e = ws.entry("app");
    let wip = branch(&e, "wip");
    assert_eq!(wip.relation, Relation::Untracked);
    assert_eq!(wip.upstream, None);
    assert_eq!(wip.unique_commits, 2);
    assert_eq!(wip.newest_commit_at, at);
    assert_eq!(wip.verdict, Verdict::LocalOnly);
}

#[test]
fn a_merged_branch_is_cleanup_unless_checked_out() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // `done` sits on a commit origin has; `fresh` is checked out with nothing
    // committed yet
    ws.git(&app, &["branch", "done"]);
    ws.git(&app, &["checkout", "-q", "-b", "fresh"]);
    for b in ["done", "fresh"] {
        ws.assert_upstream(&app, b, "");
        ws.assert_count(&app, &[b, "--not", "--remotes"], 0);
    }
    ws.assert_head(&app, Some("fresh"));

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let done = branch(&e, "done");
    assert_eq!(done.relation, Relation::Untracked);
    assert_eq!(
        done.verdict,
        Verdict::Cleanup {
            reason: CleanupReason::Merged,
            removable_worktree: None
        }
    );
    assert_eq!(branch(&e, "fresh").verdict, Verdict::Quiet);
}

#[test]
fn a_gone_upstream_with_and_without_unique_commits() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // `squashed`: pushed, then deleted upstream with its commit landing
    // nowhere (a squash merge); `merged`: fast-forwarded into main, then
    // deleted
    for b in ["squashed", "merged"] {
        ws.git(&app, &["checkout", "-q", "-b", b, "main"]);
        ws.commit(&app, &format!("work-{b}"));
        ws.git(&app, &["push", "-q", "-u", "origin", b]);
    }
    ws.git(&app, &["checkout", "-q", "main"]);
    let up = ws.upstream("app");
    ws.git(&up, &["fetch", "-q", "origin"]);
    ws.git(&up, &["merge", "-q", "--ff-only", "origin/merged"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    ws.upstream_delete_branch("app", "squashed");
    ws.upstream_delete_branch("app", "merged");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.git(&app, &["merge", "-q", "--ff-only", "origin/main"]);
    ws.assert_track(&app, "squashed", "[gone]");
    ws.assert_track(&app, "merged", "[gone]");
    ws.assert_track(&app, "main", "");
    ws.assert_count(&app, &["squashed", "--not", "--remotes"], 1);
    ws.assert_count(&app, &["merged", "--not", "--remotes"], 0);
    ws.assert_clean(&app);

    let e = ws.entry("app");
    for (b, unique) in [("squashed", 1), ("merged", 0)] {
        let s = branch(&e, b);
        assert_eq!(s.relation, Relation::Gone, "{b}");
        assert_eq!(s.unique_commits, unique, "{b}");
        assert_eq!(s.upstream.as_deref(), Some(&*format!("origin/{b}")));
        assert_eq!(
            s.verdict,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None
            },
            "{b}"
        );
    }
}

#[test]
fn a_followed_branch_whose_upstream_is_gone_needs_a_human_never_cleanup() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "branch = \"master\"");
    // the remote's default was `master`, its clone following it, with a
    // clean linked worktree on it besides the primary on `side`
    let up = ws.upstream("app");
    ws.git(&up, &["push", "-q", "origin", "main:master"]);
    let app = ws.clone_owned("app", "app", &["--branch", "master"]);
    ws.git(&app, &["checkout", "-q", "-b", "side"]);
    let wt = ws.dir("app-master");
    ws.add_worktree(&app, &wt, &["master"]);
    // renamed upstream to `main` alone
    ws.upstream_delete_branch("app", "master");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "master", "[gone]");
    ws.assert_count(&app, &["master", "--not", "--remotes"], 0);
    ws.assert_clean(&wt);

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::DefaultBranchGone {
            branch: "master".into()
        }]
    );
    let master = branch(&e, "master");
    assert_eq!(master.relation, Relation::Gone);
    // neither the branch nor the worktree it's in is offered for removal
    assert_eq!(master.verdict, Verdict::Quiet);
    // a local commit on it is local work
    ws.commit(&wt, "local");
    let e = ws.entry("app");
    assert_eq!(branch(&e, "master").verdict, Verdict::LocalOnly);
    assert_eq!(e.needs_human.len(), 1, "{:?}", e.needs_human);
}

#[test]
fn a_branch_tracking_another_remote_is_untracked_and_quiet() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.remote("vendor", &[]);
    let vendor_url = format!("file://{}", ws.bare("vendor").display());
    ws.git(&app, &["remote", "add", "upstream", &vendor_url]);
    ws.git(&app, &["fetch", "-q", "upstream"]);
    ws.git(
        &app,
        &["branch", "-q", "--track", "vendored", "upstream/main"],
    );
    ws.assert_upstream(&app, "vendored", "refs/remotes/upstream/main");
    ws.assert_track(&app, "vendored", "");
    ws.assert_count(&app, &["vendored", "--not", "--remotes"], 0);

    let e = ws.entry("app");
    let vendored = branch(&e, "vendored");
    assert_eq!(vendored.relation, Relation::Untracked);
    assert_eq!(vendored.upstream.as_deref(), Some("upstream/main"));
    assert_eq!(vendored.unique_commits, 0);
    // an upstream elsewhere isn't merged work
    assert_eq!(vendored.verdict, Verdict::Quiet);
}

#[test]
fn a_dirty_checkout_holds_a_fast_forward_but_not_a_push() {
    let mut ws = FixtureWorkspace::new();
    for name in ["behind", "ahead"] {
        ws.remote(name, &[("tracked.txt", "one\n")]);
        ws.declare_repo(name, name, "");
        ws.clone_owned(name, name, &[]);
    }
    // behind: main behind and checked out dirty; `feat` ahead elsewhere
    let behind = ws.dir("behind");
    ws.upstream_commit("behind", "feat");
    ws.upstream_commit("behind", "main");
    ws.git(&behind, &["fetch", "-q", "origin"]);
    ws.git(&behind, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.git(&behind, &["checkout", "-q", "feat"]);
    ws.commit(&behind, "local-feat");
    ws.git(&behind, &["checkout", "-q", "main"]);
    support::write(&behind, "tracked.txt", "two\n");
    ws.assert_head(&behind, Some("main"));
    ws.assert_track(&behind, "main", "[behind 1]");
    ws.assert_track(&behind, "feat", "[ahead 1]");
    ws.assert_porcelain(&behind, &[" M tracked.txt"]);
    // ahead: main ahead and checked out dirty
    let ahead = ws.dir("ahead");
    ws.commit(&ahead, "local");
    support::write(&ahead, "tracked.txt", "two\n");
    ws.assert_track(&ahead, "main", "[ahead 1]");
    ws.assert_porcelain(&ahead, &[" M tracked.txt"]);

    let entries = ws.status();
    let behind = find_entry(&entries, "behind");
    assert!(behind.needs_human.is_empty(), "{:?}", behind.needs_human);
    assert_eq!(
        branch(behind, "main").verdict,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::DirtyCheckout
        }
    );
    assert_eq!(
        branch(behind, "feat").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
    // a push only moves refs, so the dirty checkout doesn't hold it
    let ahead = find_entry(&entries, "ahead");
    assert_eq!(
        branch(ahead, "main").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
}

#[test]
fn a_symlinked_root_still_holds_a_dirty_primary_as_dirty() {
    // git reports `%(worktreepath)` resolved, so under a symlinked root it
    // differs from the checkout path the probe was given; the hold matches
    // the primary by branch name, not path
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("tracked.txt", "one\n")]);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    support::write(&app, "tracked.txt", "two\n");
    ws.assert_track(&app, "main", "[behind 1]");
    ws.assert_porcelain(&app, &[" M tracked.txt"]);
    let link = ws.outside("ws-link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();

    let entries = ws.status_at(&link, false);
    let e = find_entry(&entries, "app");
    let linked = link.join("app");
    assert_eq!(e.checkouts[0].path, linked.to_str().unwrap());
    let main = branch(e, "main");
    assert_eq!(main.worktree.as_deref(), Some(app.to_str().unwrap()));
    assert_ne!(main.worktree.as_deref(), Some(linked.to_str().unwrap()));
    assert_eq!(
        main.verdict,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::DirtyCheckout
        }
    );
}
