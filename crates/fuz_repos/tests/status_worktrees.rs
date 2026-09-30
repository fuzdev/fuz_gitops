//! Linked worktrees: each one probed as a checkout of its own — its HEAD,
//! its dirt, its operation in progress — and what that means for the
//! branches checked out in it.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;

use fuz_repos::classify::{NeedsHuman, Refresh};
use fuz_repos::probe::RegistryDirs;
use fuz_repos::sessions::LiveSessions;
use fuz_repos::state::{
    Checkout, CleanupReason, GitDirHolds, Head, HeldBy, InProgressOp, Prune, PruneLoss, Relation,
    SyncAction, Uncommitted, UnprobedHead, UnprobedWhy, UnprobedWorktree, Verdict,
};
use fuz_repos::status::{StatusOptions, status};
use support::{FixtureWorkspace, Unseal, branch, find_entry, unprobed_facts};

const fn ff(commits: u32) -> SyncAction {
    SyncAction::FastForward { commits }
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}

/// A gone worktree's git dir holding nothing that isn't elsewhere.
const NOTHING_HELD: GitDirHolds = GitDirHolds {
    submodules: false,
    worktree_refs: false,
    staged: Some(false),
};

/// `app` with a tracked file, clean on `main`.
fn app(ws: &mut FixtureWorkspace) -> std::path::PathBuf {
    let app = ws.owned_repo("app", &[("tracked.txt", "one\n")]);
    ws.assert_clean(&app);
    app
}

/// Creates `name` in `app` tracking `origin/<name>` and one commit behind it,
/// not checked out anywhere.
fn behind_branch(ws: &FixtureWorkspace, app: &Path, name: &str) {
    ws.upstream_commit("app", name);
    ws.git(app, &["fetch", "-q", "origin"]);
    ws.git(
        app,
        &["branch", "-q", "--track", name, &format!("origin/{name}")],
    );
    ws.upstream_commit("app", name);
    ws.git(app, &["fetch", "-q", "origin"]);
    ws.assert_track(app, name, "[behind 1]");
}

/// Creates `name` in `app` from `main` and pushes it with an upstream,
/// leaving it checked out nowhere.
fn pushed_branch(ws: &FixtureWorkspace, app: &Path, name: &str) {
    ws.git(app, &["branch", "-q", name, "main"]);
    ws.git(app, &["push", "-q", "-u", "origin", name]);
    ws.assert_track(app, name, "");
    ws.assert_upstream(app, name, &format!("refs/remotes/origin/{name}"));
}

#[test]
fn a_clean_linked_worktree_leaves_its_branch_to_sync() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    let wt = ws.dir("app-feat");
    ws.add_worktree(&app, &wt, &["feat"]);
    ws.assert_head(&wt, Some("feat"));
    ws.assert_clean(&wt);

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    assert_eq!(e.checkouts.len(), 2);
    assert!(e.checkouts[0].primary);
    assert_eq!(
        e.checkouts[1],
        Checkout {
            path: path(&wt),
            primary: false,
            head: Head::Branch {
                name: "feat".into()
            },
            uncommitted: Uncommitted::default(),
            in_progress: None,
            locked: false,
            linked: true,
            // not on a branch whose upstream is gone: not checked
            submodules: None,
            busy: vec![],
            working: vec![],
        }
    );
    let feat = branch(&e, "feat");
    assert_eq!(feat.worktree.as_deref(), Some(&*path(&wt)));
    // probed and clean: no longer held as an unknown
    assert_eq!(feat.verdict, Verdict::Act { action: ff(1) });
}

#[test]
fn a_dirty_linked_worktree_holds_a_fast_forward_but_not_a_push() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    pushed_branch(&ws, &app, "pushy");
    let feat_wt = ws.dir("app-feat");
    ws.add_worktree(&app, &feat_wt, &["feat"]);
    support::write(&feat_wt, "tracked.txt", "two\n");
    support::write(&feat_wt, "scratch.txt", "x\n");
    let pushy_wt = ws.dir("app-pushy");
    ws.add_worktree(&app, &pushy_wt, &["pushy"]);
    ws.commit(&pushy_wt, "local-pushy");
    support::write(&pushy_wt, "scratch.txt", "x\n");
    ws.assert_porcelain(&feat_wt, &[" M tracked.txt", "?? scratch.txt"]);
    ws.assert_porcelain(&pushy_wt, &["?? scratch.txt"]);
    ws.assert_track(&app, "feat", "[behind 1]");
    ws.assert_track(&app, "pushy", "[ahead 1]");
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(e.checkouts[0].uncommitted.is_clean());
    let feat_checkout = e.checkouts.iter().find(|c| c.path == path(&feat_wt));
    assert_eq!(
        feat_checkout.map(|c| c.uncommitted),
        Some(Uncommitted {
            unstaged: 1,
            untracked: 1,
            ..Uncommitted::default()
        })
    );
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::DirtyCheckout
        }
    );
    // a push only moves refs, so the dirty worktree it's in doesn't hold it
    assert_eq!(
        branch(&e, "pushy").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
    assert_eq!(branch(&e, "main").verdict, Verdict::Quiet);
}

#[test]
fn a_rebase_in_a_linked_worktree_holds_the_entry_but_not_the_primarys_detach() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    let wt = ws.dir("app-fix");
    let admin = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    // main ahead, so there's an action for the rebase to hold
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git(&app, &["checkout", "-q", "--detach"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    assert!(admin.join("rebase-merge").is_dir());
    assert!(!app.join(".git/rebase-merge").exists());
    ws.assert_head(&wt, None);
    ws.assert_head(&app, None);
    ws.assert_porcelain(&wt, &["UU a.txt"]);
    ws.assert_clean(&app);
    ws.assert_track(&app, "main", "[ahead 1]");

    let e = ws.entry("app");
    // the rebase is the linked worktree's; the primary's detach has no
    // operation to explain it
    assert_eq!(
        e.needs_human,
        [
            NeedsHuman::OperationInProgress {
                checkout: path(&wt),
                op: InProgressOp::Rebase,
            },
            NeedsHuman::UnexpectedDetached {
                checkout: path(&app)
            },
        ]
    );
    assert_eq!(e.checkouts[0].in_progress, None);
    assert_eq!(e.checkouts[1].in_progress, Some(InProgressOp::Rebase));
    assert_eq!(e.checkouts[1].uncommitted.conflicted, 1);
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
}

#[test]
fn a_detached_linked_worktree_is_no_reason() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("app-look");
    ws.add_worktree(&app, &wt, &["--detach"]);
    ws.assert_head(&wt, None);
    ws.assert_head(&app, Some("main"));

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(matches!(e.checkouts[1].head, Head::Detached { .. }));
    assert!(!e.checkouts[1].primary);
}

#[test]
fn a_worktree_whose_dir_is_gone_holds_its_branch_as_unprobed() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    behind_branch(&ws, &app, "hollow");
    behind_branch(&ws, &app, "usb");
    // deleted by hand: git calls it prunable
    let gone = ws.dir("app-gone");
    let gone_admin = ws.add_worktree(&app, &gone, &["feat"]);
    std::fs::remove_dir_all(&gone).unwrap();
    // its dir still there, with work staged, but its `.git` file gone: git
    // calls it prunable too, and pruning would delete its index and HEAD
    let hollow = ws.dir("app-hollow");
    let hollow_admin = ws.add_worktree(&app, &hollow, &["hollow"]);
    support::write(&hollow, "staged.txt", "keep\n");
    ws.git(&hollow, &["add", "staged.txt"]);
    std::fs::remove_file(hollow.join(".git")).unwrap();
    assert!(hollow.is_dir());
    // locked, with its `.git` file gone: never prunable, and not missing
    behind_branch(&ws, &app, "held");
    let locked_hollow = ws.dir("app-locked-hollow");
    let locked_hollow_admin = ws.add_worktree(&app, &locked_hollow, &["held"]);
    ws.git(&app, &["worktree", "lock", locked_hollow.to_str().unwrap()]);
    std::fs::remove_file(locked_hollow.join(".git")).unwrap();
    assert!(
        !ws.worktree_record(&app, &locked_hollow)
            .iter()
            .any(|l| l.starts_with("prunable"))
    );
    for wt in [&gone, &hollow] {
        let record = ws.worktree_record(&app, wt);
        assert!(
            record.iter().any(|l| l.starts_with("prunable")),
            "{record:?}"
        );
    }
    // locked on media that's unmounted: never prunable, and just as gone
    let usb = ws.outside("usb/app");
    let usb_admin = ws.add_worktree(&app, &usb, &["usb"]);
    ws.git(
        &app,
        &[
            "worktree",
            "lock",
            "--reason",
            "on usb",
            usb.to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(ws.outside("usb")).unwrap();
    let record = ws.worktree_record(&app, &usb);
    assert!(record.contains(&"locked on usb".to_owned()), "{record:?}");
    assert!(
        !record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    // can't be observed as checkouts, but they're still facts
    assert_eq!(e.checkouts.len(), 1);
    let unprobed = |(path, admin): (&Path, &Path), branch: &str, locked: bool, why: UnprobedWhy| {
        UnprobedWorktree {
            path: path.to_str().unwrap().to_owned(),
            git_dir: Some(admin.to_str().unwrap().to_owned()),
            head: UnprobedHead::Branch {
                name: branch.to_owned(),
            },
            locked,
            in_progress: None,
            // a gone one's git dir is read: it holds nothing of its own
            holds: (why == UnprobedWhy::Prunable).then_some(NOTHING_HELD),
            why,
        }
    };
    let no_git = |path: &Path| UnprobedWhy::Failed {
        error: format!("{} has no .git", path.display()),
    };
    // git lists them in its own order (by admin dir name)
    let mut listed = unprobed_facts(&e);
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        listed,
        [
            unprobed((&usb, &usb_admin), "usb", true, UnprobedWhy::Missing),
            unprobed((&gone, &gone_admin), "feat", false, UnprobedWhy::Prunable),
            // never prunable while its files are there
            unprobed((&hollow, &hollow_admin), "hollow", false, no_git(&hollow)),
            unprobed(
                (&locked_hollow, &locked_hollow_admin),
                "held",
                true,
                no_git(&locked_hollow),
            ),
        ]
    );
    // only the gone one is pruned, and it's safe: on a branch that exists
    let prunes: Vec<(&str, Option<&Prune>)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| (u.worktree.path.as_str(), u.prune.as_ref()))
        .collect();
    let gone_path = path(&gone);
    for (p, prune) in prunes {
        let want = (p == gone_path).then_some(&Prune::Safe);
        assert_eq!(prune, want, "{p}");
    }
    for b in ["feat", "hollow", "usb", "held"] {
        assert_eq!(
            branch(&e, b).verdict,
            Verdict::Held {
                action: ff(1),
                by: HeldBy::UnprobedWorktree
            },
            "{b}"
        );
    }
}

#[test]
fn a_locked_worktree_is_probed() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    let wt = ws.dir("app-locked");
    ws.add_worktree(&app, &wt, &["feat"]);
    ws.git(&app, &["worktree", "lock", wt.to_str().unwrap()]);
    support::write(&wt, "tracked.txt", "two\n");
    assert!(ws.worktree_record(&app, &wt).contains(&"locked".to_owned()));
    ws.assert_porcelain(&wt, &[" M tracked.txt"]);

    let e = ws.entry("app");
    assert_eq!(e.checkouts.len(), 2);
    assert_eq!(e.checkouts[1].path, path(&wt));
    assert!(e.checkouts[1].locked);
    assert!(!e.checkouts[0].locked);
    assert_eq!(e.checkouts[1].uncommitted.unstaged, 1);
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::DirtyCheckout
        }
    );
}

#[test]
fn a_gone_branch_in_a_clean_linked_worktree_is_removable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let gone_branches = ["old", "old-dirty", "old-locked", "old-picking"];
    for b in gone_branches {
        pushed_branch(&ws, &app, b);
        ws.upstream_delete_branch("app", b);
    }
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    // merged, with no upstream: checked out, it reads as a fresh branch
    ws.git(&app, &["branch", "-q", "done", "main"]);
    let old = ws.dir("app-old");
    ws.add_worktree(&app, &old, &["old"]);
    let old_dirty = ws.dir("app-old-dirty");
    ws.add_worktree(&app, &old_dirty, &["old-dirty"]);
    support::write(&old_dirty, "notes.txt", "keep me\n");
    let done = ws.dir("app-done");
    ws.add_worktree(&app, &done, &["done"]);
    // clean, but `git worktree remove` refuses a locked worktree
    let locked = ws.dir("app-old-locked");
    ws.add_worktree(&app, &locked, &["old-locked"]);
    ws.git(&app, &["worktree", "lock", locked.to_str().unwrap()]);
    // clean, but a cherry-pick stopped mid-way (it came out empty)
    let picking = ws.dir("app-old-picking");
    let picking_admin = ws.add_worktree(&app, &picking, &["old-picking"]);
    ws.git_fails(&picking, &["cherry-pick", "HEAD"]);
    assert!(picking_admin.join("CHERRY_PICK_HEAD").is_file());
    for b in gone_branches {
        ws.assert_track(&app, b, "[gone]");
    }
    ws.assert_clean(&locked);
    ws.assert_clean(&picking);
    ws.assert_upstream(&app, "done", "");
    ws.assert_count(&app, &["done", "--not", "--remotes"], 0);
    ws.assert_clean(&old);
    ws.assert_clean(&done);
    ws.assert_porcelain(&old_dirty, &["?? notes.txt"]);

    let e = ws.entry("app");
    assert_eq!(
        branch(&e, "old").verdict,
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: Some(path(&old)),
        }
    );
    // not removable: its dirt shows as uncommitted instead; git refuses
    // the locked one; the cherry-pick is a reason of its own
    for b in ["old-dirty", "old-locked", "old-picking"] {
        assert_eq!(
            branch(&e, b).verdict,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None,
            },
            "{b}"
        );
    }
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&picking),
            op: InProgressOp::CherryPick
        }]
    );
    assert_eq!(branch(&e, "done").verdict, Verdict::Quiet);
    // on a gone branch, but locked or mid-operation: its index isn't read
    let submodules = |p: &Path| {
        e.checkouts
            .iter()
            .find(|c| c.path == path(p))
            .map(|c| c.submodules)
    };
    assert_eq!(submodules(&locked), Some(None));
    assert_eq!(submodules(&picking), Some(None));
    assert_eq!(submodules(&old), Some(Some(false)));
    // dirty: not worth the index read
    assert_eq!(submodules(&old_dirty), Some(None));
}

#[test]
fn a_worktree_that_is_another_entrys_dir_is_never_removable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    for b in ["old", "stray"] {
        pushed_branch(&ws, &app, b);
        ws.upstream_delete_branch("app", b);
    }
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    // two entries share the repo: `app_old`'s dir is a linked worktree of
    // `app`'s, on a gone branch — otherwise just like `app-stray`, which no
    // entry claims
    let old = ws.dir("app-old");
    ws.add_worktree(&app, &old, &["old"]);
    ws.declare_repo("app_old", "app", "dir = \"app-old\"");
    let stray = ws.dir("app-stray");
    ws.add_worktree(&app, &stray, &["stray"]);
    for (b, wt) in [("old", &old), ("stray", &stray)] {
        ws.assert_track(&app, b, "[gone]");
        ws.assert_clean(wt);
        assert!(!ws.worktree_record(&app, wt).iter().any(|l| l == "locked"));
    }

    // through a symlinked root too: registry dirs compare canonicalized,
    // against git's resolved worktree paths
    let link = ws.outside("ws-link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();
    for root in [ws.root(), link] {
        let entries = ws.status_at(&root, false);
        let e = find_entry(&entries, "app");
        assert_eq!(
            branch(e, "old").verdict,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None,
            },
            "{}",
            root.display()
        );
        assert_eq!(
            branch(e, "stray").verdict,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: Some(path(&stray)),
            }
        );
    }

    let entries = ws.status();
    let e = find_entry(&entries, "app");
    // both probed alike: only the registry tells them apart
    for wt in [&old, &stray] {
        let c = e.checkouts.iter().find(|c| c.path == path(wt)).unwrap();
        assert!(c.linked && !c.locked && c.in_progress.is_none());
        assert_eq!(c.submodules, Some(false));
    }
    // seen from the other entry, `app`'s dir is the main worktree: never
    // removable either way
    let other = find_entry(&entries, "app_old");
    assert_eq!(other.checkouts[1].path, path(&app));
    assert!(!other.checkouts[1].linked);
}

#[test]
fn a_symlinked_root_reports_git_paths_for_worktrees() {
    // the primary's path is the symlinked root joined with its dir; the
    // worktrees' are git's, resolved, and the verdicts don't depend on
    // either
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "clean");
    behind_branch(&ws, &app, "dirty");
    let clean = ws.dir("app-clean");
    ws.add_worktree(&app, &clean, &["clean"]);
    let dirty = ws.dir("app-dirty");
    ws.add_worktree(&app, &dirty, &["dirty"]);
    support::write(&dirty, "tracked.txt", "two\n");
    ws.assert_clean(&clean);
    ws.assert_porcelain(&dirty, &[" M tracked.txt"]);
    let link = ws.outside("ws-link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();

    let entries = ws.status_at(&link, false);
    let e = find_entry(&entries, "app");
    assert_eq!(e.checkouts[0].path, path(&link.join("app")));
    let linked: Vec<&str> = e.checkouts[1..].iter().map(|c| c.path.as_str()).collect();
    assert_eq!(linked, [path(&clean), path(&dirty)]);
    assert_eq!(branch(e, "clean").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        branch(e, "dirty").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::DirtyCheckout
        }
    );
}

#[test]
fn a_local_status_writes_nothing_to_a_linked_worktrees_git_dirs() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    // on a branch whose upstream is gone, so the probe reads its index too
    pushed_branch(&ws, &app, "feat");
    ws.upstream_delete_branch("app", "feat");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "feat", "[gone]");
    let wt = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &wt, &["feat"]);
    ws.assert_clean(&wt);
    // same content, new stat info: an index refresh would rewrite the
    // worktree's own index
    let stale = std::time::UNIX_EPOCH + std::time::Duration::from_secs(support::CLOCK_START);
    support::set_mtime(&wt.join("a.txt"), stale);
    // the common dir holds the worktree's admin dir, `worktrees/<id>`
    let common = app.join(".git");
    assert!(admin.starts_with(&common));
    let before = support::snapshot_git_dir(&common);

    let e = ws.entry("app");
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    assert_eq!(e.checkouts.len(), 2);
    assert!(e.checkouts[1].uncommitted.is_clean());
    // the index read (`ls-files`) ran under the snapshot too
    assert_eq!(e.checkouts[1].submodules, Some(false));
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&common));

    // control: plain `git status` in the worktree does refresh its index
    let index = std::fs::read(admin.join("index")).unwrap();
    ws.git(&wt, &["status", "--porcelain"]);
    assert_ne!(
        std::fs::read(admin.join("index")).unwrap(),
        index,
        "control: plain status should rewrite the stale index"
    );
}

#[test]
fn a_linked_worktree_whose_probe_fails_is_reported_and_the_entry_stands() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "broken");
    behind_branch(&ws, &app, "fine");
    // a truncated index: git lists the worktree, but can't read its status
    let broken = ws.dir("app-broken");
    let admin = ws.add_worktree(&app, &broken, &["broken"]);
    std::fs::write(admin.join("index"), "junk").unwrap();
    ws.git_fails(&broken, &["status", "--porcelain"]);
    // a `.git` file pointing at another repo's worktree: git would happily
    // report that repo's state as this worktree's
    let astray = ws.dir("app-astray");
    ws.add_worktree(&app, &astray, &["-b", "astray"]);
    ws.remote("other", &[]);
    let other = ws.clone_owned("other", "other", &[]);
    let other_admin = ws.add_worktree(&other, &ws.dir("other-wt"), &["-b", "elsewhere"]);
    std::fs::write(
        astray.join(".git"),
        format!("gitdir: {}\n", other_admin.display()),
    )
    .unwrap();
    ws.assert_head(&astray, Some("elsewhere"));
    let fine = ws.dir("app-fine");
    ws.add_worktree(&app, &fine, &["fine"]);

    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    let failed: Vec<(&str, Option<&str>, &str)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| &u.worktree)
        .map(|u| match (&u.why, &u.head) {
            (UnprobedWhy::Failed { error }, UnprobedHead::Branch { name }) => {
                (u.path.as_str(), Some(name.as_str()), error.as_str())
            }
            (why, head) => panic!("{}: {why:?} {head:?}", u.path),
        })
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    let (astray_path, astray_branch, astray_error) = failed[0];
    assert_eq!(
        (astray_path, astray_branch),
        (&*path(&astray), Some("astray"))
    );
    assert!(
        astray_error.contains("doesn't point at this repo's git dir for it"),
        "{astray_error}"
    );
    let (broken_path, broken_branch, broken_error) = failed[1];
    assert_eq!(
        (broken_path, broken_branch),
        (&*path(&broken), Some("broken"))
    );
    assert!(
        broken_error.contains("index file smaller than expected"),
        "{broken_error}"
    );
    // the rest of the entry stands: the primary and the healthy worktree
    let probed: Vec<&str> = e.checkouts.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(probed, [path(&app), path(&fine)]);
    assert_eq!(branch(&e, "fine").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        branch(&e, "broken").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::UnprobedWorktree
        }
    );
}

#[test]
fn worktrees_are_listed_only_when_the_repo_has_some() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let entries = ws.entries();
    let spawns = || {
        let git = ws.runner();
        let run = status(
            &entries,
            &RegistryDirs::new(&ws.root(), &entries),
            &ws.root(),
            &git,
            StatusOptions {
                refresh: Refresh::Unasked,
                unregistered: None,
                fetch: false,
                jobs: 1,
                visibility_base: None,
                live: &LiveSessions::Known(vec![]),
            },
        );
        assert_eq!(run.entries[0].probe_error, None);
        git.spawns()
    };
    assert!(!app.join(".git/worktrees").exists());
    // rev-parse, config, the fetch URL, status, for-each-ref
    assert_eq!(spawns(), 5);
    // detached, so no new branch adds its own call
    let wt = ws.dir("app-look");
    ws.add_worktree(&app, &wt, &["--detach"]);
    // plus the list and the worktree's status
    assert_eq!(spawns(), 7);
    // a gone one: no status, but its index is compared with its HEAD
    let gone = ws.outside("app-gone");
    ws.add_worktree(&app, &gone, &["--detach"]);
    std::fs::remove_dir_all(&gone).unwrap();
    assert_eq!(spawns(), 8);
    // a locked one gone (unmounted media): nothing of it is at stake, so
    // nothing is read
    let usb = ws.outside("usb");
    ws.add_worktree(&app, &usb, &["--detach"]);
    ws.git(&app, &["worktree", "lock", usb.to_str().unwrap()]);
    std::fs::remove_dir_all(&usb).unwrap();
    assert_eq!(spawns(), 8);
}

#[test]
fn a_branch_on_head_in_two_checkouts_is_held_by_either() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[behind 1]");
    // `main` checked out twice more, forced
    let dup_1 = ws.dir("app-dup-1");
    ws.add_worktree(&app, &dup_1, &["-f", "main"]);
    let dup_2 = ws.dir("app-dup-2");
    ws.add_worktree(&app, &dup_2, &["-f", "main"]);
    for c in [&app, &dup_1, &dup_2] {
        ws.assert_head(c, Some("main"));
    }
    // `%(worktreepath)` names only one of the three. Dirty a linked one it
    // doesn't name and keep the primary clean: a match by that path alone,
    // or a first match (the primary), would miss the dirt
    let named = ws.git(
        &app,
        &[
            "for-each-ref",
            "--format=%(worktreepath)",
            "refs/heads/main",
        ],
    );
    let dirty = [&dup_1, &dup_2]
        .into_iter()
        .find(|d| path(d) != named)
        .unwrap();
    support::write(dirty, "tracked.txt", "two\n");
    ws.assert_porcelain(dirty, &[" M tracked.txt"]);
    ws.assert_clean(&app);
    // a gone branch in two clean worktrees
    pushed_branch(&ws, &app, "old");
    ws.upstream_delete_branch("app", "old");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "old", "[gone]");
    let old_1 = ws.dir("app-old-1");
    ws.add_worktree(&app, &old_1, &["old"]);
    let old_2 = ws.dir("app-old-2");
    ws.add_worktree(&app, &old_2, &["-f", "old"]);
    ws.assert_clean(&old_1);
    ws.assert_clean(&old_2);

    let e = ws.entry("app");
    let main = branch(&e, "main");
    assert_eq!(main.worktree.as_deref(), Some(named.as_str()));
    assert_eq!(
        main.verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::DirtyCheckout
        }
    );
    // neither worktree is the one to remove with the branch
    assert_eq!(
        branch(&e, "old").verdict,
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: None,
        }
    );
}

#[test]
fn an_operation_in_a_worktree_that_is_gone_is_still_a_reason() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    // a locked worktree on removable media, stopped mid-rebase, then
    // unmounted
    let wt = ws.outside("usb/app-fix");
    let admin = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    ws.git(&app, &["worktree", "lock", wt.to_str().unwrap()]);
    let detached_at = ws.git(&wt, &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(ws.outside("usb")).unwrap();
    assert!(admin.join("rebase-merge").is_dir());
    let record = ws.worktree_record(&app, &wt);
    assert!(record.contains(&"detached".to_owned()), "{record:?}");
    assert!(record.contains(&"locked".to_owned()), "{record:?}");
    assert!(
        !record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );
    ws.assert_track(&app, "main", "[ahead 1]");

    let e = ws.entry("app");
    assert_eq!(
        unprobed_facts(&e),
        [UnprobedWorktree {
            path: path(&wt),
            git_dir: Some(path(&admin)),
            head: UnprobedHead::Detached {
                commit: detached_at
            },
            locked: true,
            in_progress: Some(InProgressOp::Rebase),
            why: UnprobedWhy::Missing,
            holds: None,
        }]
    );
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&wt),
            op: InProgressOp::Rebase,
        }]
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
}

#[test]
fn a_registry_dir_that_is_itself_a_linked_worktree_sees_the_main_one() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("a.txt", "a\n")]);
    ws.declare_repo("app", "app", "");
    // the main worktree lives beside it, unregistered
    let main_wt = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    let admin = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    // the main worktree stops mid-merge
    ws.git(&main_wt, &["checkout", "-q", "-b", "side"]);
    support::write(&main_wt, "a.txt", "side\n");
    ws.git(&main_wt, &["commit", "-q", "-am", "side"]);
    ws.git(&main_wt, &["checkout", "-q", "main"]);
    support::write(&main_wt, "a.txt", "main\n");
    ws.git(&main_wt, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&main_wt, &["merge", "-q", "side"]);
    assert!(main_wt.join(".git/MERGE_HEAD").is_file());
    assert!(!admin.join("MERGE_HEAD").exists());
    assert!(app.join(".git").is_file());
    ws.assert_porcelain(&main_wt, &["UU a.txt"]);
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    let checkouts: Vec<(&str, bool)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.primary))
        .collect();
    assert_eq!(checkouts, [(&*path(&app), true), (&*path(&main_wt), false)]);
    assert_eq!(e.checkouts[0].in_progress, None);
    assert_eq!(e.checkouts[1].in_progress, Some(InProgressOp::Merge));
    assert_eq!(e.checkouts[1].uncommitted.conflicted, 1);
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&main_wt),
            op: InProgressOp::Merge,
        }]
    );
}

#[test]
fn a_fetch_from_a_linked_worktree_counts_as_the_repos_fetch() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &wt, &["-b", "feat"]);
    let primary_fetch = app.join(".git/FETCH_HEAD");
    assert!(!primary_fetch.exists());
    // no fetch yet: the clone's own reflog entry dates it
    assert_eq!(ws.entry("app").fetched_at, Some(ws.clone_reflog_time(&app)));
    // each worktree fetches into its own git dir
    ws.git(&wt, &["fetch", "-q", "origin"]);
    let linked_fetch = admin.join("FETCH_HEAD");
    assert!(linked_fetch.is_file());
    assert!(!primary_fetch.exists());
    let at = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    support::set_mtime(&linked_fetch, at(support::CLOCK_START + 1000));
    assert_eq!(
        ws.entry("app").fetched_at,
        Some(support::CLOCK_START + 1000)
    );
    // the newest across the worktrees wins, whichever it is
    ws.git(&app, &["fetch", "-q", "origin"]);
    support::set_mtime(&primary_fetch, at(support::CLOCK_START));
    assert_eq!(
        ws.entry("app").fetched_at,
        Some(support::CLOCK_START + 1000)
    );
    support::set_mtime(&primary_fetch, at(support::CLOCK_START + 2000));
    assert_eq!(
        ws.entry("app").fetched_at,
        Some(support::CLOCK_START + 2000)
    );
}

#[test]
fn a_linked_primary_is_dated_by_its_repos_clone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("a.txt", "a\n")]);
    ws.declare_repo("app", "app", "");
    let main_wt = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    let admin = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    // never fetched, from either
    assert!(!admin.join("FETCH_HEAD").exists());
    assert!(!main_wt.join(".git/FETCH_HEAD").exists());
    // the primary's own reflog starts with its worktree's creation, not a
    // clone: the clone's entry is the common dir's alone
    let own = std::fs::read_to_string(admin.join("logs/HEAD")).unwrap();
    assert!(!own.lines().next().unwrap().contains("\tclone: "), "{own}");

    let e = ws.entry("app");
    assert!(e.checkouts[0].primary && e.checkouts[0].linked);
    assert_eq!(e.fetched_at, Some(ws.clone_reflog_time(&main_wt)));
}

#[test]
fn a_worktree_that_cannot_be_looked_at_is_a_failure_not_gone() {
    use std::os::unix::fs::PermissionsExt;
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    pushed_branch(&ws, &app, "wip");
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    let sealed = ws.outside("sealed");
    std::fs::create_dir(&sealed).unwrap();
    let wt = sealed.join("app-feat");
    ws.add_worktree(&app, &wt, &["feat"]);
    let wip = sealed.join("app-wip");
    ws.add_worktree(&app, &wip, &["wip"]);
    ws.commit(&wip, "wip");
    ws.assert_track(&app, "wip", "[ahead 1]");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _unseal = Unseal(sealed.clone());
    if std::fs::read_dir(&sealed).is_ok() {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    // git itself reads the unreachable worktree as prunable
    let record = ws.worktree_record(&app, &wt);
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let e = ws.entry("app");
    let paths: Vec<&str> = e
        .unprobed_worktrees
        .iter()
        .map(|u| u.worktree.path.as_str())
        .collect();
    assert_eq!(paths, [path(&wt), path(&wip)]);
    for u in &e.unprobed_worktrees {
        match &u.worktree.why {
            UnprobedWhy::Failed { error } => {
                assert!(error.contains("Permission denied"), "{error}");
            }
            why => panic!("{why:?}"),
        }
    }
    // nor can whether a live session works in them be told: each holds its
    // own branch, pushes included, and the entry's other branches act
    let unresolvable = |p: &Path| NeedsHuman::CheckoutUnresolvable {
        checkout: path(p),
        path: path(p),
        error: "Permission denied (os error 13)".into(),
    };
    assert_eq!(e.needs_human, [unresolvable(&wt), unresolvable(&wip)]);
    // unprobed, as before busy detection: the fast-forward's hold
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::UnprobedWorktree
        }
    );
    assert_eq!(
        branch(&e, "wip").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::BusyUnknown
        }
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
}

#[test]
fn a_worktree_git_does_not_list_is_still_a_fact() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    behind_branch(&ws, &app, "blank");
    // its git dir's `gitdir` file deleted: git drops it from the list and
    // from `%(worktreepath)`, though it's there and dirty
    let wt = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &wt, &["feat"]);
    support::write(&wt, "tracked.txt", "two\n");
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    // an empty `gitdir` file, the same
    let blank = ws.dir("app-blank");
    let blank_admin = ws.add_worktree(&app, &blank, &["blank"]);
    std::fs::write(blank_admin.join("gitdir"), "").unwrap();
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(
        !list.contains("app-feat") && !list.contains("app-blank"),
        "{list}"
    );
    for b in ["feat", "blank"] {
        let named = ws.git(
            &app,
            &[
                "for-each-ref",
                "--format=%(worktreepath)",
                &format!("refs/heads/{b}"),
            ],
        );
        assert_eq!(named, "", "{b}");
    }
    ws.assert_porcelain(&wt, &[" M tracked.txt"]);

    let e = ws.entry("app");
    let mut unlisted = unprobed_facts(&e);
    unlisted.sort_by(|a, b| a.path.cmp(&b.path));
    // with no readable `gitdir`, the path is the worktree's own git dir
    assert_eq!(
        unlisted,
        [
            UnprobedWorktree {
                path: path(&blank_admin),
                git_dir: Some(path(&blank_admin)),
                head: UnprobedHead::Branch {
                    name: "blank".into()
                },
                locked: false,
                in_progress: None,
                why: UnprobedWhy::Failed {
                    error: format!(
                        "not listed by git: {} is empty",
                        blank_admin.join("gitdir").display()
                    ),
                },
                holds: None,
            },
            UnprobedWorktree {
                path: path(&admin),
                git_dir: Some(path(&admin)),
                head: UnprobedHead::Branch {
                    name: "feat".into()
                },
                locked: false,
                in_progress: None,
                why: UnprobedWhy::Failed {
                    error: format!(
                        "not listed by git: reading {}: No such file or directory (os error 2)",
                        admin.join("gitdir").display()
                    ),
                },
                holds: None,
            },
        ]
    );
    for b in ["feat", "blank"] {
        assert_eq!(
            branch(&e, b).verdict,
            Verdict::Held {
                action: ff(1),
                by: HeldBy::UnprobedWorktree
            },
            "{b}"
        );
    }
}

#[test]
fn an_unreadable_worktree_git_dir_holds_the_entry() {
    use std::os::unix::fs::PermissionsExt;
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    behind_branch(&ws, &app, "feat");
    pushed_branch(&ws, &app, "pushy");
    ws.git(&app, &["checkout", "-q", "pushy"]);
    ws.commit(&app, "local-pushy");
    ws.git(&app, &["checkout", "-q", "main"]);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[behind 1]");
    ws.assert_track(&app, "pushy", "[ahead 1]");
    let wt = ws.dir("app-sealed");
    let admin = ws.add_worktree(&app, &wt, &["--detach"]);
    std::fs::set_permissions(&admin, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _unseal = Unseal(admin.clone());
    if std::fs::read_dir(&admin).is_ok() {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-sealed"), "{list}");
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    let u = &e.unprobed_worktrees[0].worktree;
    assert_eq!(u.path, path(&admin));
    // its HEAD can't be read: it might be on any branch
    assert_eq!(u.head, UnprobedHead::Unknown);
    match &u.why {
        UnprobedWhy::Failed { error } => {
            assert!(error.starts_with("not listed by git: reading"), "{error}");
            assert!(error.contains("Permission denied"), "{error}");
        }
        why => panic!("{why:?}"),
    }
    // an operation there can't be ruled out: the entry is held, pushes too
    assert_eq!(
        e.needs_human,
        [NeedsHuman::WorktreeUnreadable { path: path(&admin) }]
    );
    for (b, action) in [
        ("main", ff(1)),
        ("feat", ff(1)),
        ("pushy", SyncAction::Push { commits: 1 }),
    ] {
        assert_eq!(
            branch(&e, b).verdict,
            Verdict::Held {
                action,
                by: HeldBy::Entry
            },
            "{b}"
        );
    }
}

#[test]
fn the_main_worktree_is_never_removable() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("a.txt", "a\n")]);
    ws.declare_repo("app", "app", "");
    let main_wt = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    // the main worktree sits, clean, on a branch whose upstream is gone
    pushed_branch(&ws, &main_wt, "old");
    ws.upstream_delete_branch("app", "old");
    ws.git(&main_wt, &["fetch", "-q", "--prune", "origin"]);
    ws.git(&main_wt, &["checkout", "-q", "old"]);
    ws.assert_track(&main_wt, "old", "[gone]");
    ws.assert_clean(&main_wt);

    let e = ws.entry("app");
    assert!(e.checkouts[0].primary && e.checkouts[0].linked);
    assert_eq!(e.checkouts[1].path, path(&main_wt));
    assert!(!e.checkouts[1].primary && !e.checkouts[1].linked);
    // the main worktree is never removed: its index isn't read
    assert_eq!(e.checkouts[1].submodules, None);
    // `git worktree remove` refuses the main worktree
    assert_eq!(
        branch(&e, "old").verdict,
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: None,
        }
    );
}

#[test]
fn a_separate_git_dir_is_the_primary_not_a_worktree() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    std::fs::create_dir(ws.outside("gits")).unwrap();
    let gits = ws.outside("gits/app.git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", gits.to_str().unwrap()],
    );
    assert!(app.join(".git").is_file());
    let wt = ws.dir("app-feat");
    ws.add_worktree(&app, &wt, &["-b", "feat"]);
    // git prints the main worktree's git dir as its path
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(
        list.starts_with(&format!("worktree {}\n", gits.display())),
        "{list}"
    );

    let e = ws.entry("app");
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    let checkouts: Vec<(&str, bool, bool)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.primary, c.linked))
        .collect();
    assert_eq!(
        checkouts,
        [(&*path(&app), true, false), (&*path(&wt), false, true)]
    );
}

#[test]
fn a_registry_dir_linked_to_a_bare_repo() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let bare = ws.outside("app.git");
    let url = format!("file://{}", ws.bare("app").display());
    ws.git(
        ws.base(),
        &["clone", "-q", "--bare", &url, bare.to_str().unwrap()],
    );
    ws.set_origin(&bare, "app", &support::owned_origin("app"));
    let app = ws.dir("app");
    ws.add_worktree(&bare, &app, &["-b", "work"]);
    ws.git(&bare, &["worktree", "lock", app.to_str().unwrap()]);
    assert!(
        ws.worktree_record(&bare, &bare)
            .contains(&"bare".to_owned())
    );
    assert!(
        ws.worktree_record(&bare, &app)
            .contains(&"locked".to_owned())
    );
    // a fetch in the bare repo writes the common dir's FETCH_HEAD
    ws.git(&bare, &["fetch", "-q", "origin"]);
    let fetch_head = bare.join("FETCH_HEAD");
    assert!(fetch_head.is_file());
    let at = support::CLOCK_START + 1000;
    support::set_mtime(
        &fetch_head,
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(at),
    );

    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    // the bare record has no files to probe
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    assert_eq!(e.checkouts.len(), 1);
    assert!(e.checkouts[0].linked && e.checkouts[0].locked);
    assert_eq!(e.fetched_at, Some(at));
}

/// `app` with `main` one behind origin and `pushy` one ahead, both clean in
/// the primary, so a hold on each kind of action shows.
fn behind_and_ahead(ws: &mut FixtureWorkspace) -> std::path::PathBuf {
    let app = app(ws);
    pushed_branch(ws, &app, "pushy");
    ws.git(&app, &["checkout", "-q", "pushy"]);
    ws.commit(&app, "local-pushy");
    ws.git(&app, &["checkout", "-q", "main"]);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[behind 1]");
    ws.assert_track(&app, "pushy", "[ahead 1]");
    ws.assert_clean(&app);
    app
}

#[test]
fn a_listed_worktree_whose_head_git_cannot_read_holds_every_fast_forward() {
    let mut ws = FixtureWorkspace::new();
    let app = behind_and_ahead(&mut ws);
    let null = format!("HEAD {}", "0".repeat(40));
    // its HEAD deleted: git lists it as the null id, `detached`
    let missing = ws.dir("app-missing-head");
    let missing_admin = ws.add_worktree(&app, &missing, &["-b", "missing"]);
    std::fs::remove_file(missing_admin.join("HEAD")).unwrap();
    let record = ws.worktree_record(&app, &missing);
    assert!(
        record.contains(&null) && record.contains(&"detached".to_owned()),
        "{record:?}"
    );
    // its HEAD garbled: the null id, no head line at all
    let garbled = ws.dir("app-garbled-head");
    let garbled_admin = ws.add_worktree(&app, &garbled, &["-b", "garbled"]);
    std::fs::write(garbled_admin.join("HEAD"), "garbage\n").unwrap();
    let record = ws.worktree_record(&app, &garbled);
    assert!(record.contains(&null), "{record:?}");
    assert!(
        !record
            .iter()
            .any(|l| l.starts_with("branch") || l == "detached"),
        "{record:?}"
    );
    for b in ["missing", "garbled"] {
        let named = ws.git(
            &app,
            &[
                "for-each-ref",
                "--format=%(worktreepath)",
                &format!("refs/heads/{b}"),
            ],
        );
        assert_eq!(named, "", "{b}");
    }

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let heads: Vec<(&str, &UnprobedHead)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| (u.worktree.path.as_str(), &u.worktree.head))
        .collect();
    assert_eq!(heads.len(), 2, "{heads:?}");
    for (p, head) in heads {
        assert!(p == path(&missing) || p == path(&garbled), "{p}");
        assert_eq!(*head, UnprobedHead::Unknown, "{p}");
    }
    // either might be on any branch: none reads as merged
    for b in ["missing", "garbled"] {
        assert_eq!(branch(&e, b).verdict, Verdict::Quiet, "{b}");
    }
    // every fast-forward is held, pushes act
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::UnprobedWorktree
        }
    );
    assert_eq!(
        branch(&e, "pushy").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
}

#[test]
fn an_unreadable_worktrees_dir_holds_the_entry() {
    use std::os::unix::fs::PermissionsExt;
    // 000 and 311: `worktrees/` can't be listed; 644: it can, but nothing
    // in it can be looked at, so the admin dir is what's unreadable
    for mode in [0o000, 0o311, 0o644] {
        let mut ws = FixtureWorkspace::new();
        let app = behind_and_ahead(&mut ws);
        behind_branch(&ws, &app, "feat");
        let admin = ws.add_worktree(&app, &ws.dir("app-feat"), &["feat"]);
        let worktrees = app.join(".git/worktrees");
        std::fs::set_permissions(&worktrees, std::fs::Permissions::from_mode(mode)).unwrap();
        let _unseal = Unseal(worktrees.clone());
        if std::fs::read_dir(&worktrees).is_ok() && std::fs::metadata(&admin).is_ok() {
            eprintln!("skipped: permissions don't bind this user (root)");
            return;
        }
        // git lists no linked worktree at all, and names none for `feat`
        let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
        assert!(!list.contains("app-feat"), "{mode:o}: {list}");
        let named = ws.git(
            &app,
            &[
                "for-each-ref",
                "--format=%(worktreepath)",
                "refs/heads/feat",
            ],
        );
        assert_eq!(named, "", "{mode:o}");

        let unreadable = if mode == 0o644 { &admin } else { &worktrees };
        let e = ws.entry("app");
        assert_eq!(
            e.needs_human,
            [NeedsHuman::WorktreeUnreadable {
                path: path(unreadable)
            }],
            "{mode:o}"
        );
        for (b, action) in [
            ("main", ff(1)),
            ("feat", ff(1)),
            ("pushy", SyncAction::Push { commits: 1 }),
        ] {
            assert_eq!(
                branch(&e, b).verdict,
                Verdict::Held {
                    action,
                    by: HeldBy::Entry
                },
                "{mode:o} {b}"
            );
        }
    }
}

#[test]
fn an_unreadable_worktrees_dir_withholds_every_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // `merged`: nothing unique, no upstream, checked out in a worktree;
    // `gone`: its upstream deleted, a commit on no remote
    ws.git(&app, &["branch", "-q", "merged"]);
    ws.add_worktree(&app, &ws.dir("app-merged"), &["merged"]);
    pushed_branch(&ws, &app, "gone");
    ws.git(&app, &["checkout", "-q", "gone"]);
    ws.commit(&app, "local-gone");
    ws.git(&app, &["checkout", "-q", "main"]);
    ws.upstream_delete_branch("app", "gone");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "gone", "[gone]");
    ws.assert_clean(&app);
    let worktrees = app.join(".git/worktrees");
    std::fs::set_permissions(&worktrees, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unseal = Unseal(worktrees.clone());
    if std::fs::read_dir(&worktrees).is_ok() {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    // git no longer knows `merged` is checked out anywhere
    let named = ws.git(
        &app,
        &[
            "for-each-ref",
            "--format=%(worktreepath)",
            "refs/heads/merged",
        ],
    );
    assert_eq!(named, "");

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::WorktreeUnreadable {
            path: path(&worktrees)
        }]
    );
    assert_eq!(branch(&e, "merged").verdict, Verdict::Quiet);
    assert_eq!(branch(&e, "gone").relation, Relation::Gone);
    assert_eq!(branch(&e, "gone").verdict, Verdict::LocalOnly);
    // readable again, each is cleanup as ever
    drop(unseal);
    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(matches!(
        branch(&e, "gone").verdict,
        Verdict::Cleanup { .. }
    ));
}

#[test]
fn an_unreadable_admin_dir_withholds_cleanup_of_gone_branches() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // each checked out in its own worktree, its upstream deleted: `gone`
    // with a commit on no remote, `gone0` with none
    let mut admins = Vec::new();
    for (name, commits) in [("gone", 1), ("gone0", 0)] {
        pushed_branch(&ws, &app, name);
        let wt = ws.dir(&format!("app-{name}"));
        admins.push(ws.add_worktree(&app, &wt, &[name]));
        for i in 0..commits {
            ws.commit(&wt, &format!("{name}-{i}"));
        }
        ws.upstream_delete_branch("app", name);
    }
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "gone", "[gone]");
    ws.assert_track(&app, "gone0", "[gone]");
    ws.assert_count(&app, &["gone", "--not", "--remotes"], 1);
    ws.assert_count(&app, &["gone0", "--not", "--remotes"], 0);
    let e = ws.entry("app");
    for b in ["gone", "gone0"] {
        assert!(
            matches!(branch(&e, b).verdict, Verdict::Cleanup { .. }),
            "{b}"
        );
    }
    // one admin dir sealed: its worktree's HEAD is unknown, so it may be
    // on either branch, and `git branch -D` would strand it
    let Some(_sealed) = support::seal(&admins[0], 0o000) else {
        return;
    };
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-gone\n"), "{list}");

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::WorktreeUnreadable {
            path: path(&admins[0])
        }]
    );
    let u = e
        .unprobed_worktrees
        .iter()
        .find(|u| u.worktree.path == path(&admins[0]))
        .unwrap();
    assert_eq!(u.worktree.head, UnprobedHead::Unknown);
    assert_eq!(branch(&e, "gone").relation, Relation::Gone);
    assert_eq!(branch(&e, "gone").verdict, Verdict::LocalOnly);
    assert_eq!(branch(&e, "gone0").verdict, Verdict::Quiet);
}

#[test]
fn a_worktree_with_initialized_submodules_is_not_removable() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("sub", &[]);
    let app = app(&mut ws);
    // `plain` branches off before the submodule exists
    pushed_branch(&ws, &app, "plain");
    let sub_url = format!("file://{}", ws.bare("sub").display());
    ws.git(&app, &["submodule", "add", "-q", &sub_url, "sub"]);
    ws.git(&app, &["commit", "-q", "-m", "sub"]);
    let with_sub = ["initialized", "deinited", "cloned", "declared"];
    for b in with_sub {
        pushed_branch(&ws, &app, b);
    }
    for b in std::iter::once("plain").chain(with_sub) {
        ws.upstream_delete_branch("app", b);
    }
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    let mut admins = Vec::new();
    for b in ["plain", "initialized", "deinited", "cloned", "declared"] {
        ws.assert_track(&app, b, "[gone]");
        admins.push(ws.add_worktree(&app, &ws.dir(&format!("app-{b}")), &[b]));
    }
    let initialized = ws.dir("app-initialized");
    ws.git(&initialized, &["submodule", "update", "--init", "-q"]);
    // initialized, then deinitialized: `modules/` stays, and so does git's
    // refusal
    let deinited = ws.dir("app-deinited");
    ws.git(&deinited, &["submodule", "update", "--init", "-q"]);
    ws.git(&deinited, &["submodule", "deinit", "-q", "--all"]);
    assert!(!deinited.join("sub/.git").exists());
    // populated by hand, never through git: no `modules/`, still refused
    let cloned = ws.dir("app-cloned");
    std::fs::remove_dir(cloned.join("sub")).unwrap();
    ws.git(&cloned, &["clone", "-q", &sub_url, "sub"]);
    // git refuses to remove a worktree once a submodule was initialized in
    // it; declared but never initialized, it removes it
    for admin in &admins[1..3] {
        assert!(admin.join("modules").is_dir(), "{}", admin.display());
    }
    for admin in &admins[3..] {
        assert!(!admin.join("modules").exists(), "{}", admin.display());
    }
    assert!(cloned.join("sub/.git").is_dir());
    assert!(ws.dir("app-declared/.gitmodules").is_file());
    for b in ["plain", "initialized", "deinited", "cloned", "declared"] {
        ws.assert_clean(&ws.dir(&format!("app-{b}")));
    }

    let e = ws.entry("app");
    let removable = |b: &str| match &branch(&e, b).verdict {
        Verdict::Cleanup {
            removable_worktree, ..
        } => removable_worktree.clone(),
        v => panic!("{b}: {v:?}"),
    };
    assert_eq!(removable("plain"), Some(path(&ws.dir("app-plain"))));
    for b in ["initialized", "deinited", "cloned"] {
        assert_eq!(removable(b), None, "{b}");
    }
    assert_eq!(removable("declared"), Some(path(&ws.dir("app-declared"))));
    let submodules = |p: &Path| {
        e.checkouts
            .iter()
            .find(|c| c.path == path(p))
            .map(|c| c.submodules)
    };
    assert_eq!(submodules(&initialized), Some(Some(true)));
    assert_eq!(submodules(&ws.dir("app-declared")), Some(Some(false)));
}

#[test]
fn an_unlisted_worktree_mid_rebase_holds_the_entry() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    let wt = ws.dir("app-fix");
    let admin = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    assert!(admin.join("rebase-merge").is_dir());
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-fix"), "{list}");
    ws.assert_track(&app, "main", "[ahead 1]");

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&admin),
            op: InProgressOp::Rebase,
        }]
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
}

#[test]
fn a_linked_primary_git_does_not_list_is_not_its_own_worktree() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let main_wt = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    let admin = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    let list = ws.git(&main_wt, &["worktree", "list", "--porcelain"]);
    assert!(
        !list.contains(&format!("worktree {}\n", app.display())),
        "{list}"
    );
    ws.assert_head(&app, Some("work"));

    let e = ws.entry("app");
    assert!(
        e.unprobed_worktrees.is_empty(),
        "{:?}",
        e.unprobed_worktrees
    );
    let checkouts: Vec<(&str, bool)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.primary))
        .collect();
    assert_eq!(checkouts, [(&*path(&app), true), (&*path(&main_wt), false)]);
}

#[test]
fn stray_entries_under_worktrees() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let worktrees = app.join(".git/worktrees");
    std::fs::create_dir(&worktrees).unwrap();
    // a file there is skipped, as git skips it
    std::fs::write(worktrees.join("stray-file"), "x").unwrap();
    // an empty git dir holds no worktree, but fails closed with a hint
    let empty = worktrees.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert_eq!(list.matches("worktree ").count(), 1, "{list}");

    let e = ws.entry("app");
    assert_eq!(
        unprobed_facts(&e),
        [UnprobedWorktree {
            path: path(&empty),
            git_dir: Some(path(&empty)),
            head: UnprobedHead::Unknown,
            locked: false,
            in_progress: None,
            why: UnprobedWhy::Failed {
                error: format!(
                    "not listed by git: {} holds no worktree (no gitdir, no HEAD); \
                     delete that dir by hand",
                    empty.display()
                ),
            },
            holds: None,
        }]
    );
}

#[test]
fn a_copied_git_dir_serves_one_worktree() {
    // the copy sorting before and after the original, so taking the first
    // admin dir that claims the path goes wrong in one of them
    for copy_name in ["aaa", "zzz"] {
        copied_git_dir_serves_one_worktree(copy_name, "b2");
    }
    // on the same branch: the two records are alike, and each admin dir
    // still serves one
    copied_git_dir_serves_one_worktree("aaa", "b1");
}

fn copied_git_dir_serves_one_worktree(copy_name: &str, copy_head: &str) {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.git(&app, &["branch", "-q", "b2", "main"]);
    let wt = ws.dir("app-wt");
    let admin = ws.add_worktree(&app, &wt, &["-b", "b1"]);
    // a copy of its git dir, claiming the same path, on another branch
    let copy = admin.with_file_name(copy_name);
    let status = std::process::Command::new("cp")
        .arg("-r")
        .arg(&admin)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(copy.join("HEAD"), format!("ref: refs/heads/{copy_head}\n")).unwrap();
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        list.matches(&format!("worktree {}\n", wt.display()))
            .count(),
        2,
        "{list}"
    );

    let e = ws.entry("app");
    // the worktree once, with its own HEAD
    let at_wt: Vec<&Head> = e
        .checkouts
        .iter()
        .filter(|c| c.path == path(&wt))
        .map(|c| &c.head)
        .collect();
    assert_eq!(at_wt, [&Head::Branch { name: "b1".into() }], "{copy_name}");
    // the copy's record fails its `.git` check, with the copy's HEAD
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    let u = &e.unprobed_worktrees[0].worktree;
    assert_eq!(u.path, path(&wt));
    assert_eq!(
        u.head,
        UnprobedHead::Branch {
            name: copy_head.into()
        },
        "{copy_name}"
    );
    assert!(
        matches!(&u.why, UnprobedWhy::Failed { error }
            if error.contains("doesn't point at this repo's git dir")),
        "{:?}",
        u.why
    );
}

#[test]
fn pruning_a_gone_detached_worktree_would_lose_its_commit() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("app-spike");
    let admin = ws.add_worktree(&app, &wt, &["--detach"]);
    let spike = ws.commit(&wt, "spike");
    std::fs::remove_dir_all(&wt).unwrap();
    // its HEAD is the only ref to the commit
    let containing = ws.git(&app, &["for-each-ref", "--contains", &spike]);
    assert_eq!(containing, "");
    let record = ws.worktree_record(&app, &wt);
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let e = ws.entry("app");
    assert_eq!(
        unprobed_facts(&e),
        [UnprobedWorktree {
            path: path(&wt),
            git_dir: Some(path(&admin)),
            head: UnprobedHead::Detached { commit: spike },
            locked: false,
            in_progress: None,
            why: UnprobedWhy::Prunable,
            holds: Some(NOTHING_HELD),
        }]
    );
    assert_eq!(
        e.unprobed_worktrees[0].prune,
        Some(Prune::Loses {
            losses: vec![PruneLoss::DetachedHead]
        })
    );
}

#[test]
fn pruning_a_worktree_moved_mid_rebase_would_lose_the_rebase() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    let wt = ws.dir("app-fix");
    let admin = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    // moved by hand: git calls the old path prunable
    std::fs::rename(&wt, ws.outside("moved-fix")).unwrap();
    assert!(admin.join("rebase-merge").is_dir());
    let record = ws.worktree_record(&app, &wt);
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let e = ws.entry("app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    let u = &e.unprobed_worktrees[0];
    assert_eq!(
        (&u.worktree.why, u.worktree.in_progress),
        (&UnprobedWhy::Prunable, Some(InProgressOp::Rebase))
    );
    // the rebase detached its HEAD, too, and its conflicted index differs
    // from that HEAD
    assert_eq!(
        u.prune,
        Some(Prune::Loses {
            losses: vec![
                PruneLoss::Operation {
                    op: InProgressOp::Rebase
                },
                PruneLoss::DetachedHead,
                PruneLoss::StagedChanges,
            ]
        })
    );
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&wt),
            op: InProgressOp::Rebase,
        }]
    );
}

#[test]
fn a_committed_nested_repo_blocks_removal_without_gitmodules() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    pushed_branch(&ws, &app, "old");
    ws.upstream_delete_branch("app", "old");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    let wt = ws.dir("app-old");
    ws.add_worktree(&app, &wt, &["old"]);
    // a repo nested and committed as a gitlink, never declared
    let nested = wt.join("nested");
    ws.git(&wt, &["init", "-q", "nested"]);
    ws.git(&nested, &["commit", "-q", "--allow-empty", "-m", "nested"]);
    ws.git(&wt, &["add", "nested"]);
    ws.git(&wt, &["commit", "-q", "-m", "embed"]);
    assert!(!wt.join(".gitmodules").exists());
    let staged = ws.git(&wt, &["ls-files", "--stage", "nested"]);
    assert!(staged.starts_with("160000 "), "{staged}");
    ws.assert_clean(&wt);
    ws.assert_track(&app, "old", "[gone]");

    let e = ws.entry("app");
    let c = e.checkouts.iter().find(|c| c.path == path(&wt)).unwrap();
    assert_eq!(c.submodules, Some(true));
    // `git worktree remove` refuses it
    assert_eq!(
        branch(&e, "old").verdict,
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: None,
        }
    );
}

#[test]
fn the_index_is_read_only_for_a_worktree_on_a_gone_branch() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // three clean linked worktrees: on a fresh branch, on a pushed one, and
    // on one whose upstream is gone
    pushed_branch(&ws, &app, "pushed");
    pushed_branch(&ws, &app, "old");
    ws.upstream_delete_branch("app", "old");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "old", "[gone]");
    ws.assert_track(&app, "pushed", "");
    ws.add_worktree(&app, &ws.dir("app-fresh"), &["-b", "fresh"]);
    for b in ["pushed", "old"] {
        ws.add_worktree(&app, &ws.dir(&format!("app-{b}")), &[b]);
    }
    let entries = ws.entries();
    let git = ws.runner();
    let run = status(
        &entries,
        &RegistryDirs::new(&ws.root(), &entries),
        &ws.root(),
        &git,
        StatusOptions {
            refresh: Refresh::Unasked,
            unregistered: None,
            fetch: false,
            jobs: 1,
            visibility_base: None,
            live: &LiveSessions::Known(vec![]),
        },
    );
    let e = &run.entries[0];
    // rev-parse, config, the fetch URL, status, for-each-ref; rev-list for
    // `fresh` and `old` (they could carry local work); the worktree list and
    // three statuses; and one `ls-files`, for the worktree on `old`
    assert_eq!(git.spawns(), 5 + 2 + 1 + 3 + 1);
    let submodules = |b: &str| {
        e.checkouts
            .iter()
            .find(|c| c.path == path(&ws.dir(&format!("app-{b}"))))
            .map(|c| c.submodules)
    };
    assert_eq!(submodules("old"), Some(Some(false)));
    assert_eq!(submodules("fresh"), Some(None));
    assert_eq!(submodules("pushed"), Some(None));
}

#[test]
fn pruning_a_gone_worktree_whose_branch_was_deleted_would_lose_its_commit() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("app-feat");
    ws.add_worktree(&app, &wt, &["-b", "feat"]);
    let work = ws.commit(&wt, "work");
    std::fs::remove_dir_all(&wt).unwrap();
    ws.git(&app, &["update-ref", "-d", "refs/heads/feat"]);
    // the worktree's HEAD still names `feat`, the only ref to the commit
    assert!(!ws.has_ref(&app, "refs/heads/feat"));
    assert_eq!(ws.git(&app, &["for-each-ref", "--contains", &work]), "");
    let record = ws.worktree_record(&app, &wt);
    assert!(
        record.contains(&"branch refs/heads/feat".to_owned()),
        "{record:?}"
    );
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let e = ws.entry("app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    let u = &e.unprobed_worktrees[0];
    assert_eq!(u.worktree.why, UnprobedWhy::Prunable);
    assert_eq!(
        u.prune,
        Some(Prune::Loses {
            losses: vec![PruneLoss::MissingBranch {
                name: "feat".into()
            }]
        })
    );
}

#[test]
fn a_gitdir_written_without_its_git_suffix_names_the_worktree_itself() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let k = ws.outside("k");
    let admin = ws.add_worktree(&app, &k, &["-b", "k"]);
    // by hand: git takes a `gitdir` without `/.git` as the worktree's path
    std::fs::write(admin.join("gitdir"), format!("{}\n", k.display())).unwrap();
    assert_eq!(
        ws.worktree_record(&app, &k)[0],
        format!("worktree {}", k.display())
    );

    let e = ws.entry("app");
    let paths: Vec<&str> = e.checkouts.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, [path(&app).as_str(), path(&k).as_str()]);
    assert_eq!(unprobed_facts(&e), []);
}

#[test]
fn a_worktree_whose_git_is_a_fifo_fails_its_probe_without_blocking() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.outside("app-fifo");
    ws.add_worktree(&app, &wt, &["-b", "fifo"]);
    std::fs::remove_file(wt.join(".git")).unwrap();
    let out = ws
        .command("mkfifo", ws.base())
        .arg(wt.join(".git"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ws.entry("app"));
    });
    let done = rx.recv_timeout(std::time::Duration::from_secs(60));
    assert!(done.is_ok(), "blocked: not finished within a minute");
    let e = done.unwrap();
    let u = unprobed_facts(&e);
    assert_eq!(u.len(), 1, "{u:?}");
    assert_eq!(u[0].path, path(&wt));
    match &u[0].why {
        UnprobedWhy::Failed { error } => assert!(error.contains("not a regular file"), "{error}"),
        why => panic!("{why:?}"),
    }
}

#[test]
fn no_gone_worktree_is_safe_to_remove_beside_a_relative_gitdir() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // `k` is live with staged work, its git dir naming it relatively: git
    // 2.48+ resolves that against the git dir, older gits against the cwd,
    // and read against the cwd it's gone
    let k = ws.dir("k");
    let k_admin = ws.add_worktree(&app, &k, &["-b", "k"]);
    std::fs::write(k_admin.join("gitdir"), "../../../../k/.git\n").unwrap();
    assert_eq!(k_admin.join("../../../../k").canonicalize().unwrap(), k);
    support::write(&k, "l.txt", "live\n");
    ws.git(&k, &["add", "l.txt"]);
    ws.assert_porcelain(&k, &["A  l.txt"]);
    // `g` deleted, on a branch that exists: otherwise safe
    let g = ws.outside("g");
    ws.add_worktree(&app, &g, &["-b", "g"]);
    std::fs::remove_dir_all(&g).unwrap();

    let e = ws.entry("app");
    let relative = Prune::Loses {
        losses: vec![PruneLoss::RelativeGitdir {
            git_dir: path(&k_admin),
        }],
    };
    let gone = e
        .unprobed_worktrees
        .iter()
        .find(|u| u.worktree.path == path(&g))
        .unwrap();
    assert_eq!(gone.worktree.why, UnprobedWhy::Prunable);
    assert_eq!(gone.prune.as_ref(), Some(&relative));
    // whichever way this git reads `k`, nothing is safe to remove
    let k_loss = PruneLoss::RelativeGitdir {
        git_dir: path(&k_admin),
    };
    for u in &e.unprobed_worktrees {
        if u.worktree.why == UnprobedWhy::Prunable {
            assert!(
                matches!(&u.prune, Some(Prune::Loses { losses }) if losses.contains(&k_loss)),
                "{u:?}"
            );
        }
    }
}

#[test]
fn a_gone_worktrees_git_dir_can_hold_what_removing_it_loses() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let file_ok = ["-c", "protocol.file.allow=always"];
    // a submodule, committed and pushed
    ws.remote("sub", &[("s.txt", "s\n")]);
    let sub_url = format!("file://{}", ws.bare("sub").display());
    ws.git(
        &app,
        &[&file_ok[..], &["submodule", "add", "-q", &sub_url, "sub"]].concat(),
    );
    ws.git(&app, &["commit", "-q", "-m", "add sub"]);
    ws.git(&app, &["push", "-q", "origin", "main"]);
    // `sm`: its submodule initialized, with a commit only there
    let sm = ws.outside("sm");
    let sm_admin = ws.add_worktree(&app, &sm, &["-b", "sm"]);
    ws.git(
        &sm,
        &[&file_ok[..], &["submodule", "update", "--init", "-q"]].concat(),
    );
    ws.commit(&sm.join("sub"), "only here");
    assert!(sm_admin.join("modules/sub").is_dir());
    // `rw`: a commit only a per-worktree ref holds
    let rw = ws.outside("rw");
    let rw_admin = ws.add_worktree(&app, &rw, &["-b", "rw"]);
    let only = ws.commit(&rw, "only here");
    ws.git(&rw, &["update-ref", "refs/worktree/keep", &only]);
    ws.git(&rw, &["reset", "-q", "--hard", "HEAD~1"]);
    assert_eq!(ws.git(&app, &["for-each-ref", "--contains", &only]), "");
    // `st`: a change staged, in no commit
    let st = ws.outside("st");
    let st_admin = ws.add_worktree(&app, &st, &["-b", "st"]);
    support::write(&st, "new.txt", "staged\n");
    ws.git(&st, &["add", "new.txt"]);
    ws.assert_porcelain(&st, &["A  new.txt"]);
    for wt in [&sm, &rw, &st] {
        std::fs::remove_dir_all(wt).unwrap();
    }
    let admins = [&sm_admin, &rw_admin, &st_admin];
    let before: Vec<_> = admins
        .iter()
        .map(|a| support::snapshot_git_dir(a))
        .collect();

    let e = ws.entry("app");
    let held = |wt: &Path| {
        let u = e
            .unprobed_worktrees
            .iter()
            .find(|u| u.worktree.path == path(wt))
            .unwrap();
        assert_eq!(u.worktree.why, UnprobedWhy::Prunable);
        (u.worktree.holds, u.prune.clone())
    };
    let loses = |loss| Some(Prune::Loses { losses: vec![loss] });
    assert_eq!(
        held(&sm),
        (
            Some(GitDirHolds {
                submodules: true,
                ..NOTHING_HELD
            }),
            loses(PruneLoss::Submodules)
        )
    );
    assert_eq!(
        held(&rw),
        (
            Some(GitDirHolds {
                worktree_refs: true,
                ..NOTHING_HELD
            }),
            loses(PruneLoss::WorktreeRefs)
        )
    );
    assert_eq!(
        held(&st),
        (
            Some(GitDirHolds {
                staged: Some(true),
                ..NOTHING_HELD
            }),
            loses(PruneLoss::StagedChanges)
        )
    );
    // read, never written: the index compare took no lock and refreshed
    // nothing
    for (admin, before) in admins.iter().zip(&before) {
        support::assert_git_dir_unchanged(before, &support::snapshot_git_dir(admin));
    }
}

#[test]
fn a_git_dir_with_no_worktree_is_deleted_only_when_it_keeps_nothing() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let worktrees = app.join(".git/worktrees");
    // `ini`: only the lock `git worktree add` writes first, which `git
    // worktree prune` skips
    let ini = worktrees.join("ini");
    std::fs::create_dir_all(&ini).unwrap();
    std::fs::write(ini.join("locked"), "initializing\n").unwrap();
    // `mods`: a submodule's repo and a per-worktree ref
    let mods = worktrees.join("mods");
    std::fs::create_dir_all(mods.join("modules/sub")).unwrap();
    std::fs::create_dir_all(mods.join("refs/worktree")).unwrap();
    std::fs::write(mods.join("refs/worktree/keep"), "0123\n").unwrap();
    // `ix`: an index, maybe with staged changes; `rb`: a rebase's state
    let ix = worktrees.join("ix");
    std::fs::create_dir_all(&ix).unwrap();
    std::fs::write(ix.join("index"), "DIRC").unwrap();
    let rb = worktrees.join("rb");
    std::fs::create_dir_all(rb.join("rebase-merge")).unwrap();
    // `bare`: an empty `refs/` tree and `modules/`, nothing in them
    let bare = worktrees.join("bare");
    std::fs::create_dir_all(bare.join("refs/worktree")).unwrap();
    std::fs::create_dir_all(bare.join("modules")).unwrap();

    let mut errors: Vec<(String, String)> = unprobed_facts(&ws.entry("app"))
        .into_iter()
        .map(|u| match u.why {
            UnprobedWhy::Failed { error } => (u.path, error),
            why => panic!("{why:?}"),
        })
        .collect();
    errors.sort();
    let no_worktree = |dir: &Path, rest: &str| {
        (
            path(dir),
            format!(
                "not listed by git: {} holds no worktree (no gitdir, no HEAD){rest}",
                dir.display()
            ),
        )
    };
    assert_eq!(
        errors,
        [
            no_worktree(&bare, "; delete that dir by hand"),
            no_worktree(
                &ini,
                " but keeps a lock (a git worktree add may be under way); check it by hand"
            ),
            no_worktree(
                &ix,
                " but keeps an index (maybe staged changes); check it by hand"
            ),
            no_worktree(
                &mods,
                " but keeps submodules' repos (modules/) and per-worktree refs (refs/); \
                 check it by hand"
            ),
            no_worktree(&rb, " but keeps a rebase in progress; check it by hand"),
        ]
    );
}

#[test]
fn what_a_gone_worktrees_index_and_refs_hold() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let gone = |name: &str, args: &[&str]| {
        let wt = ws.outside(name);
        let admin = ws.add_worktree(&app, &wt, args);
        (wt, admin)
    };
    // `ci`: an index git can't read
    let (ci, ci_admin) = gone("ci", &["-b", "ci"]);
    // `ita`: only an intent to add, which holds no content
    let (ita, _) = gone("ita", &["-b", "ita"]);
    support::write(&ita, "n.txt", "n\n");
    ws.git(&ita, &["add", "-N", "n.txt"]);
    ws.assert_porcelain(&ita, &[" A n.txt"]);
    // `nc`: added `--no-checkout`, so no index at all
    let (nc, nc_admin) = gone("nc", &["--no-checkout", "-b", "nc"]);
    assert!(!nc_admin.join("index").exists());
    // `rw`: a leftover `refs/rewritten/` ref, no operation in progress
    let (rw, rw_admin) = gone("rw", &["-b", "rw"]);
    ws.git(&rw, &["update-ref", "refs/rewritten/x", "HEAD"]);
    assert!(rw_admin.join("refs/rewritten/x").is_file());
    // `bs`: a bisect started and reset, leaving an empty `refs/bisect/`
    let (bs, bs_admin) = gone("bs", &["-b", "bs"]);
    for label in ["b1", "b2", "b3"] {
        ws.commit(&bs, label);
    }
    ws.git(&bs, &["bisect", "start", "HEAD", "HEAD~3"]);
    ws.git(&bs, &["bisect", "reset"]);
    assert!(bs_admin.join("refs/bisect").is_dir());
    assert!(
        std::fs::read_dir(bs_admin.join("refs/bisect"))
            .unwrap()
            .next()
            .is_none()
    );
    for wt in [&ci, &ita, &nc, &rw, &bs] {
        std::fs::remove_dir_all(wt).unwrap();
    }
    std::fs::write(ci_admin.join("index"), "garbage\n").unwrap();

    let e = ws.entry("app");
    let held = |wt: &Path| {
        let u = e
            .unprobed_worktrees
            .iter()
            .find(|u| u.worktree.path == path(wt))
            .unwrap();
        assert_eq!(u.worktree.why, UnprobedWhy::Prunable);
        (u.worktree.holds, u.prune.clone())
    };
    let safe = (Some(NOTHING_HELD), Some(Prune::Safe));
    // git failed on the index: that counts as staged, failing closed
    assert_eq!(
        held(&ci),
        (
            Some(GitDirHolds {
                staged: None,
                ..NOTHING_HELD
            }),
            Some(Prune::Loses {
                losses: vec![PruneLoss::StagedChanges]
            })
        )
    );
    assert_eq!(held(&ita), safe);
    assert_eq!(held(&nc), safe);
    assert_eq!(
        held(&rw),
        (
            Some(GitDirHolds {
                worktree_refs: true,
                ..NOTHING_HELD
            }),
            Some(Prune::Loses {
                losses: vec![PruneLoss::WorktreeRefs]
            })
        )
    );
    assert_eq!(held(&bs), safe);
}

#[test]
fn a_reftable_gone_worktrees_refs_are_read_from_git() {
    let mut ws = FixtureWorkspace::new();
    // a git that can't make reftable repos has nothing to read here
    let probe = ws.outside("reftable-probe");
    let made = ws.git_output(
        ws.base(),
        &[
            "init",
            "-q",
            "--ref-format=reftable",
            probe.to_str().unwrap(),
        ],
    );
    if !made.status.success() {
        eprintln!("skipped: this git makes no reftable repos");
        return;
    }
    ws.remote("app", &[("tracked.txt", "one\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--ref-format=reftable"]);
    assert_eq!(
        ws.git(&app, &["rev-parse", "--show-ref-format"]),
        "reftable"
    );
    // `plain` holds nothing of its own; `bs`, `wt`, and `rw` each a ref in
    // one of git's per-worktree namespaces
    let gone = |name: &str, per_worktree: Option<&str>| {
        let wt = ws.outside(name);
        let admin = ws.add_worktree(&app, &wt, &["-b", name]);
        if let Some(r) = per_worktree {
            ws.git(&wt, &["update-ref", r, "HEAD"]);
        }
        // every reftable worktree git dir holds a `refs/heads` stub
        assert!(admin.join("reftable").is_dir(), "{name}");
        assert!(admin.join("refs/heads").is_file(), "{name}");
        std::fs::remove_dir_all(&wt).unwrap();
        (wt, admin)
    };
    let (plain, plain_admin) = gone("plain", None);
    let (bs, _) = gone("bs", Some("refs/bisect/bad"));
    let (wt, _) = gone("wt", Some("refs/worktree/keep"));
    let (rw, _) = gone("rw", Some("refs/rewritten/x"));
    // the refs are that worktree's alone, invisible from the primary
    assert_eq!(
        ws.git(
            &app,
            &[
                "for-each-ref",
                "refs/bisect/",
                "refs/worktree/",
                "refs/rewritten/"
            ]
        ),
        ""
    );
    let before = support::snapshot_git_dir(&plain_admin);

    let e = ws.entry("app");
    let held = |wt: &Path| {
        let u = e
            .unprobed_worktrees
            .iter()
            .find(|u| u.worktree.path == path(wt))
            .unwrap();
        assert_eq!(u.worktree.why, UnprobedWhy::Prunable);
        (u.worktree.holds, u.prune.clone())
    };
    assert_eq!(held(&plain), (Some(NOTHING_HELD), Some(Prune::Safe)));
    for wt in [&bs, &wt, &rw] {
        assert_eq!(
            held(wt),
            (
                Some(GitDirHolds {
                    worktree_refs: true,
                    ..NOTHING_HELD
                }),
                Some(Prune::Loses {
                    losses: vec![PruneLoss::WorktreeRefs]
                })
            ),
            "{}",
            wt.display()
        );
    }
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&plain_admin));
    // tables git can't read count as held: git lists nothing from them,
    // and exits 0
    std::fs::write(plain_admin.join("reftable/tables.list"), "garbage\n").unwrap();
    let listed = ws.git_output(
        &app,
        &[
            &format!("--git-dir={}", plain_admin.display()),
            "for-each-ref",
            "refs/",
        ],
    );
    assert!(
        listed.status.success() && listed.stdout.is_empty(),
        "{listed:?}"
    );
    let e = ws.entry("app");
    let u = e
        .unprobed_worktrees
        .iter()
        .find(|u| u.worktree.path == path(&plain))
        .unwrap();
    assert!(u.worktree.holds.is_some_and(|h| h.worktree_refs), "{u:?}");
}

#[test]
fn a_gone_worktrees_refs_that_cannot_be_read_count_as_held() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.outside("ur");
    let admin = ws.add_worktree(&app, &wt, &["-b", "ur"]);
    std::fs::remove_dir_all(&wt).unwrap();
    std::fs::create_dir_all(admin.join("refs/worktree")).unwrap();
    let Some(_sealed) = support::seal(&admin.join("refs"), 0o000) else {
        return;
    };

    let e = ws.entry("app");
    let u = &e.unprobed_worktrees[0];
    assert_eq!(
        u.worktree.holds,
        Some(GitDirHolds {
            worktree_refs: true,
            ..NOTHING_HELD
        })
    );
    assert_eq!(
        u.prune,
        Some(Prune::Loses {
            losses: vec![PruneLoss::WorktreeRefs]
        })
    );
}

#[test]
fn a_listed_worktrees_gitfile_is_read_as_git_reads_it() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let add = |name: &str| {
        let wt = ws.dir(name);
        let admin = ws.add_worktree(&app, &wt, &["-b", name]);
        (wt, admin)
    };
    // each `.git` rewritten to name its own git dir: git follows a path cut
    // at a NUL, and one that isn't UTF-8 (a link to the git dir)
    let (nul, nul_admin) = add("nul");
    let gitfile = |named: &[u8], tail: &[u8]| [b"gitdir: ", named, tail].concat();
    let nul_gitfile = gitfile(nul_admin.as_os_str().as_bytes(), b"\0junk\n");
    std::fs::write(nul.join(".git"), nul_gitfile).unwrap();
    let (raw, raw_admin) = add("raw");
    let link = ws.base().join(std::ffi::OsStr::from_bytes(b"admin-\xff"));
    std::os::unix::fs::symlink(&raw_admin, &link).unwrap();
    let raw_gitfile = gitfile(link.as_os_str().as_bytes(), b"\n");
    std::fs::write(raw.join(".git"), raw_gitfile).unwrap();
    ws.assert_head(&nul, Some("nul"));
    ws.assert_head(&raw, Some("raw"));
    // and refuses its `gitdir: ` on a second line, or with a trailing space
    let (second, second_admin) = add("second");
    let second_gitfile = [
        b"x\n".as_slice(),
        &gitfile(second_admin.as_os_str().as_bytes(), b"\n"),
    ]
    .concat();
    std::fs::write(second.join(".git"), second_gitfile).unwrap();
    let (spaced, spaced_admin) = add("spaced");
    let spaced_gitfile = gitfile(spaced_admin.as_os_str().as_bytes(), b" \n");
    std::fs::write(spaced.join(".git"), spaced_gitfile).unwrap();
    ws.git_fails(&second, &["status"]);
    ws.git_fails(&spaced, &["status"]);
    // git lists all four
    for wt in [&nul, &raw, &second, &spaced] {
        ws.worktree_record(&app, wt);
    }

    let e = ws.entry("app");
    let mut probed: Vec<&str> = e
        .checkouts
        .iter()
        .filter(|c| c.linked)
        .map(|c| c.path.as_str())
        .collect();
    probed.sort_unstable();
    assert_eq!(probed, [path(&nul), path(&raw)]);
    let mut failed: Vec<(&str, &str)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| match &u.worktree.why {
            UnprobedWhy::Failed { error } => (u.worktree.path.as_str(), error.as_str()),
            why => panic!("{}: {why:?}", u.worktree.path),
        })
        .collect();
    failed.sort_unstable();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert_eq!(failed[0].0, path(&second));
    let invalid = format!("invalid gitfile format: {}", second.join(".git").display());
    assert_eq!(failed[0].1, invalid);
    assert_eq!(failed[1].0, path(&spaced));
    assert!(failed[1].1.contains("can't be resolved"), "{}", failed[1].1);
}

#[test]
fn an_unlisted_worktrees_head_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let commit = ws.git(&app, &["rev-parse", "main"]);
    let heads: [(&str, Vec<u8>); 4] = [
        ("nospace", b"ref:refs/heads/nospace\n".to_vec()),
        ("junk", format!("{commit} junk\n").into_bytes()),
        ("lead", b" ref: refs/heads/lead\n".to_vec()),
        ("nul", b"ref: refs/heads/nul\0junk\n".to_vec()),
    ];
    for (name, head) in &heads {
        let admin = ws.add_worktree(&app, &ws.dir(name), &["-b", name]);
        std::fs::write(admin.join("HEAD"), head).unwrap();
        // git lists no worktree whose git dir names none
        std::fs::remove_file(admin.join("gitdir")).unwrap();
    }
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    for (name, _) in &heads {
        assert!(!list.contains(&path(&ws.dir(name))), "{list}");
    }
    // git, working through each: on a branch, detached, or no repo at all
    let symref = |name: &str| ws.git(&ws.dir(name), &["symbolic-ref", "HEAD"]);
    assert_eq!(symref("nospace"), "refs/heads/nospace");
    assert_eq!(symref("nul"), "refs/heads/nul");
    assert_eq!(ws.git(&ws.dir("junk"), &["rev-parse", "HEAD"]), commit);
    ws.git_fails(&ws.dir("junk"), &["symbolic-ref", "-q", "HEAD"]);
    ws.git_fails(&ws.dir("lead"), &["rev-parse", "HEAD"]);

    let e = ws.entry("app");
    let head_of = |name: &str| {
        let suffix = format!("/worktrees/{name}");
        let found: Vec<&UnprobedHead> = e
            .unprobed_worktrees
            .iter()
            .filter(|u| u.worktree.path.ends_with(&suffix))
            .map(|u| &u.worktree.head)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", e.unprobed_worktrees);
        found[0].clone()
    };
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    assert_eq!(head_of("nospace"), on("nospace"));
    assert_eq!(head_of("nul"), on("nul"));
    assert_eq!(head_of("junk"), UnprobedHead::Detached { commit });
    assert_eq!(head_of("lead"), UnprobedHead::Unknown);
}

#[test]
fn an_unlisted_worktrees_symlinked_head_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let commit = ws.git(&app, &["rev-parse", "main"]);
    ws.git(&app, &["tag", "v1"]);
    // git itself writes a symlink under `core.preferSymlinkRefs`
    let sym = ws.dir("sym");
    ws.git(
        &app,
        &[
            "-c",
            "core.preferSymlinkRefs=true",
            "worktree",
            "add",
            "-q",
            sym.to_str().unwrap(),
            "-b",
            "sym",
        ],
    );
    let admin = |name: &str| app.join(".git/worktrees").join(name);
    let link = std::fs::read_link(admin("sym").join("HEAD")).unwrap();
    assert_eq!(link, Path::new("refs/heads/sym"));
    // the rest by hand: a tag, a loose ref read through, and an invalid ref
    // name git falls through to reading as a file
    let relink = |name: &str, to: &str| {
        let head = admin(name).join("HEAD");
        std::fs::remove_file(&head).unwrap();
        std::os::unix::fs::symlink(to, &head).unwrap();
    };
    for name in ["tag", "through", "fall"] {
        ws.add_worktree(&app, &ws.dir(name), &["-b", name]);
    }
    relink("tag", "refs/tags/v1");
    relink("through", "../../refs/heads/through");
    std::fs::create_dir_all(admin("fall").join("refs/heads")).unwrap();
    std::fs::write(
        admin("fall").join("refs/heads/x y"),
        "ref: refs/heads/fall\n",
    )
    .unwrap();
    relink("fall", "refs/heads/x y");
    // git's view of each
    let record = |name: &str| ws.worktree_record(&app, &ws.dir(name))[1..].to_vec();
    assert_eq!(
        record("sym"),
        [format!("HEAD {commit}"), "branch refs/heads/sym".into()]
    );
    assert_eq!(
        record("tag"),
        [format!("HEAD {commit}"), "branch refs/tags/v1".into()]
    );
    assert_eq!(
        record("through"),
        [format!("HEAD {commit}"), "detached".into()]
    );
    assert_eq!(
        record("fall"),
        [format!("HEAD {commit}"), "branch refs/heads/fall".into()]
    );
    // unlisted, so the tool reads each `HEAD` itself
    for name in ["sym", "tag", "through", "fall"] {
        std::fs::remove_file(admin(name).join("gitdir")).unwrap();
    }

    let e = ws.entry("app");
    let head_of = |name: &str| {
        let suffix = format!("/worktrees/{name}");
        let found: Vec<&UnprobedHead> = e
            .unprobed_worktrees
            .iter()
            .filter(|u| u.worktree.path.ends_with(&suffix))
            .map(|u| &u.worktree.head)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", e.unprobed_worktrees);
        found[0].clone()
    };
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    assert_eq!(head_of("sym"), on("sym"));
    // the tool reports only branches
    assert_eq!(head_of("tag"), UnprobedHead::Unknown);
    assert_eq!(head_of("through"), UnprobedHead::Detached { commit });
    assert_eq!(head_of("fall"), on("fall"));
}
