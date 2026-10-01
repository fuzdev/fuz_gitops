//! Linked worktrees probed as checkouts of their own: HEAD, dirt, an operation
//! in progress, and the branches checked out in them.

mod support;

use std::path::Path;

use fuz_repos::classify::{NeedsHuman, Refresh};
use fuz_repos::registry::RegistryDirs;
use fuz_repos::sessions::LiveSessions;
use fuz_repos::state::{
    BranchHold, Checkout, CleanupReason, Head, InProgressOp, Prune, SyncAction, Uncommitted,
    UnprobedWhy, UnprobedWorktree, Verdict,
};
use fuz_repos::status::{StatusOptions, status};
use support::worktrees::{NOTHING_HELD, app, behind_branch, pushed_branch};
use support::{FixtureWorkspace, branch, ff, find_entry, path, unprobed_facts};

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
            by: BranchHold::DirtyCheckout
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
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    // main ahead, so there's an action for the rebase to hold
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git(&app, &["checkout", "-q", "--detach"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    assert!(git_dir.join("rebase-merge").is_dir());
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
            by: BranchHold::Entry
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
    let gone_git_dir = ws.add_worktree(&app, &gone, &["feat"]);
    std::fs::remove_dir_all(&gone).unwrap();
    // its dir still there, with work staged, but its `.git` file gone: git
    // calls it prunable too, and pruning would delete its index and HEAD
    let hollow = ws.dir("app-hollow");
    let hollow_git_dir = ws.add_worktree(&app, &hollow, &["hollow"]);
    support::write(&hollow, "staged.txt", "keep\n");
    ws.git(&hollow, &["add", "staged.txt"]);
    std::fs::remove_file(hollow.join(".git")).unwrap();
    assert!(hollow.is_dir());
    // locked, with its `.git` file gone: never prunable, and not missing
    behind_branch(&ws, &app, "held");
    let locked_hollow = ws.dir("app-locked-hollow");
    let locked_hollow_git_dir = ws.add_worktree(&app, &locked_hollow, &["held"]);
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
    let usb_git_dir = ws.add_worktree(&app, &usb, &["usb"]);
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
    let unprobed =
        |(path, git_dir): (&Path, &Path), branch: &str, locked: bool, why: UnprobedWhy| {
            UnprobedWorktree {
                path: path.to_str().unwrap().to_owned(),
                git_dir: Some(git_dir.to_str().unwrap().to_owned()),
                head: Some(Head::Branch {
                    name: branch.to_owned(),
                }),
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
    // git lists them in its own order (by worktree git dir name)
    let mut listed = unprobed_facts(&e);
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        listed,
        [
            unprobed((&usb, &usb_git_dir), "usb", true, UnprobedWhy::Missing),
            unprobed((&gone, &gone_git_dir), "feat", false, UnprobedWhy::Prunable),
            // never prunable while its files are there
            unprobed((&hollow, &hollow_git_dir), "hollow", false, no_git(&hollow)),
            unprobed(
                (&locked_hollow, &locked_hollow_git_dir),
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
                by: BranchHold::UnprobedWorktree
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
            by: BranchHold::DirtyCheckout
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
    let picking_git_dir = ws.add_worktree(&app, &picking, &["old-picking"]);
    ws.git_fails(&picking, &["cherry-pick", "HEAD"]);
    assert!(picking_git_dir.join("CHERRY_PICK_HEAD").is_file());
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
        let entries = ws.status_at(&root);
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

    let entries = ws.status_at(&link);
    let e = find_entry(&entries, "app");
    assert_eq!(e.checkouts[0].path, path(&link.join("app")));
    let linked: Vec<&str> = e.checkouts[1..].iter().map(|c| c.path.as_str()).collect();
    assert_eq!(linked, [path(&clean), path(&dirty)]);
    assert_eq!(branch(e, "clean").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        branch(e, "dirty").verdict,
        Verdict::Held {
            action: ff(1),
            by: BranchHold::DirtyCheckout
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
    let git_dir = ws.add_worktree(&app, &wt, &["feat"]);
    ws.assert_clean(&wt);
    // same content, new stat info: an index refresh would rewrite the
    // worktree's own index
    let stale = std::time::UNIX_EPOCH + std::time::Duration::from_secs(support::CLOCK_START);
    support::set_mtime(&wt.join("a.txt"), stale);
    // the common dir holds the worktree's git dir, `worktrees/<id>`
    let common = app.join(".git");
    assert!(git_dir.starts_with(&common));
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
    let index = std::fs::read(git_dir.join("index")).unwrap();
    ws.git(&wt, &["status", "--porcelain"]);
    assert_ne!(
        std::fs::read(git_dir.join("index")).unwrap(),
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
    let git_dir = ws.add_worktree(&app, &broken, &["broken"]);
    std::fs::write(git_dir.join("index"), "junk").unwrap();
    ws.git_fails(&broken, &["status", "--porcelain"]);
    // a `.git` file pointing at another repo's worktree: git would happily
    // report that repo's state as this worktree's
    let astray = ws.dir("app-astray");
    ws.add_worktree(&app, &astray, &["-b", "astray"]);
    ws.remote("other", &[]);
    let other = ws.clone_owned("other", "other", &[]);
    let other_git_dir = ws.add_worktree(&other, &ws.dir("other-wt"), &["-b", "elsewhere"]);
    std::fs::write(
        astray.join(".git"),
        format!("gitdir: {}\n", other_git_dir.display()),
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
            (UnprobedWhy::Failed { error }, Some(Head::Branch { name })) => {
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
            by: BranchHold::UnprobedWorktree
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
            by: BranchHold::DirtyCheckout
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
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    ws.git(&app, &["worktree", "lock", wt.to_str().unwrap()]);
    let detached_at = ws.git(&wt, &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(ws.outside("usb")).unwrap();
    assert!(git_dir.join("rebase-merge").is_dir());
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
            git_dir: Some(path(&git_dir)),
            head: Some(Head::Detached {
                commit: detached_at
            }),
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
            by: BranchHold::Entry
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
    let git_dir = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    // the main worktree stops mid-merge
    ws.git(&main_wt, &["checkout", "-q", "-b", "side"]);
    support::write(&main_wt, "a.txt", "side\n");
    ws.git(&main_wt, &["commit", "-q", "-am", "side"]);
    ws.git(&main_wt, &["checkout", "-q", "main"]);
    support::write(&main_wt, "a.txt", "main\n");
    ws.git(&main_wt, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&main_wt, &["merge", "-q", "side"]);
    assert!(main_wt.join(".git/MERGE_HEAD").is_file());
    assert!(!git_dir.join("MERGE_HEAD").exists());
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
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "feat"]);
    let primary_fetch = app.join(".git/FETCH_HEAD");
    assert!(!primary_fetch.exists());
    // no fetch yet: the clone's own reflog entry dates it
    assert_eq!(ws.entry("app").fetched_at, Some(ws.clone_reflog_time(&app)));
    // each worktree fetches into its own git dir
    ws.git(&wt, &["fetch", "-q", "origin"]);
    let linked_fetch = git_dir.join("FETCH_HEAD");
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
    let git_dir = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    // never fetched, from either
    assert!(!git_dir.join("FETCH_HEAD").exists());
    assert!(!main_wt.join(".git/FETCH_HEAD").exists());
    // the primary's own reflog starts with its worktree's creation, not a
    // clone: the clone's entry is the common dir's alone
    let own = std::fs::read_to_string(git_dir.join("logs/HEAD")).unwrap();
    assert!(!own.lines().next().unwrap().contains("\tclone: "), "{own}");

    let e = ws.entry("app");
    assert!(e.checkouts[0].primary && e.checkouts[0].linked);
    assert_eq!(e.fetched_at, Some(ws.clone_reflog_time(&main_wt)));
}
