//! Worktrees git does not list or cannot be read: unreadable worktree git dirs,
//! unlisted and copied git dirs, which git dirs count as the primary rather
//! than a worktree, and the holds they put on the entry.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::state::{
    BranchHold, CleanupReason, Head, InProgressOp, Relation, SyncAction, UnprobedWhy,
    UnprobedWorktree, Verdict,
};
use support::unregistered::copy_dir;
use support::worktrees::{app, behind_branch, pushed_branch};
use support::{FixtureWorkspace, Unseal, branch, ff, path, unprobed_facts};

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
            by: BranchHold::UnprobedWorktree
        }
    );
    assert_eq!(
        branch(&e, "wip").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: BranchHold::BusyUnknown
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
    let git_dir = ws.add_worktree(&app, &wt, &["feat"]);
    support::write(&wt, "tracked.txt", "two\n");
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    // an empty `gitdir` file, the same
    let blank = ws.dir("app-blank");
    let blank_git_dir = ws.add_worktree(&app, &blank, &["blank"]);
    std::fs::write(blank_git_dir.join("gitdir"), "").unwrap();
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
                path: path(&blank_git_dir),
                git_dir: Some(path(&blank_git_dir)),
                head: Some(Head::Branch {
                    name: "blank".into()
                }),
                locked: false,
                in_progress: None,
                why: UnprobedWhy::Failed {
                    error: format!(
                        "not listed by git: {} is empty",
                        blank_git_dir.join("gitdir").display()
                    ),
                },
                holds: None,
            },
            UnprobedWorktree {
                path: path(&git_dir),
                git_dir: Some(path(&git_dir)),
                head: Some(Head::Branch {
                    name: "feat".into()
                }),
                locked: false,
                in_progress: None,
                why: UnprobedWhy::Failed {
                    error: format!(
                        "not listed by git: reading {}: No such file or directory (os error 2)",
                        git_dir.join("gitdir").display()
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
                by: BranchHold::UnprobedWorktree
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
    let git_dir = ws.add_worktree(&app, &wt, &["--detach"]);
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _unseal = Unseal(git_dir.clone());
    if std::fs::read_dir(&git_dir).is_ok() {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-sealed"), "{list}");
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    let u = &e.unprobed_worktrees[0].worktree;
    assert_eq!(u.path, path(&git_dir));
    // its HEAD can't be read: it might be on any branch
    assert_eq!(u.head, None);
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
        [NeedsHuman::WorktreeUnreadable {
            path: path(&git_dir)
        }]
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
                by: BranchHold::Entry
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
    let missing_git_dir = ws.add_worktree(&app, &missing, &["-b", "missing"]);
    std::fs::remove_file(missing_git_dir.join("HEAD")).unwrap();
    let record = ws.worktree_record(&app, &missing);
    assert!(
        record.contains(&null) && record.contains(&"detached".to_owned()),
        "{record:?}"
    );
    // its HEAD garbled: the null id, no head line at all
    let garbled = ws.dir("app-garbled-head");
    let garbled_git_dir = ws.add_worktree(&app, &garbled, &["-b", "garbled"]);
    std::fs::write(garbled_git_dir.join("HEAD"), "garbage\n").unwrap();
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
    let heads: Vec<(&str, &Option<Head>)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| (u.worktree.path.as_str(), &u.worktree.head))
        .collect();
    assert_eq!(heads.len(), 2, "{heads:?}");
    for (p, head) in heads {
        assert!(p == path(&missing) || p == path(&garbled), "{p}");
        assert_eq!(*head, None, "{p}");
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
            by: BranchHold::UnprobedWorktree
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
    // in it can be looked at, so the worktree git dir is what's unreadable
    for mode in [0o000, 0o311, 0o644] {
        let mut ws = FixtureWorkspace::new();
        let app = behind_and_ahead(&mut ws);
        behind_branch(&ws, &app, "feat");
        let git_dir = ws.add_worktree(&app, &ws.dir("app-feat"), &["feat"]);
        let worktrees = app.join(".git/worktrees");
        std::fs::set_permissions(&worktrees, std::fs::Permissions::from_mode(mode)).unwrap();
        let _unseal = Unseal(worktrees.clone());
        if std::fs::read_dir(&worktrees).is_ok() && std::fs::metadata(&git_dir).is_ok() {
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

        let unreadable = if mode == 0o644 { &git_dir } else { &worktrees };
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
                    by: BranchHold::Entry
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
fn an_unreadable_worktree_git_dir_withholds_cleanup_of_gone_branches() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // each checked out in its own worktree, its upstream deleted: `gone`
    // with a commit on no remote, `gone0` with none
    let mut git_dirs = Vec::new();
    for (name, commits) in [("gone", 1), ("gone0", 0)] {
        pushed_branch(&ws, &app, name);
        let wt = ws.dir(&format!("app-{name}"));
        git_dirs.push(ws.add_worktree(&app, &wt, &[name]));
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
    // one worktree git dir sealed: its worktree's HEAD is unknown, so it may be
    // on either branch, and `git branch -D` would strand it
    let Some(_sealed) = support::seal(&git_dirs[0], 0o000) else {
        return;
    };
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-gone\n"), "{list}");

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::WorktreeUnreadable {
            path: path(&git_dirs[0])
        }]
    );
    let u = e
        .unprobed_worktrees
        .iter()
        .find(|u| u.worktree.path == path(&git_dirs[0]))
        .unwrap();
    assert_eq!(u.worktree.head, None);
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
    let mut git_dirs = Vec::new();
    for b in ["plain", "initialized", "deinited", "cloned", "declared"] {
        ws.assert_track(&app, b, "[gone]");
        git_dirs.push(ws.add_worktree(&app, &ws.dir(&format!("app-{b}")), &[b]));
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
    for git_dir in &git_dirs[1..3] {
        assert!(git_dir.join("modules").is_dir(), "{}", git_dir.display());
    }
    for git_dir in &git_dirs[3..] {
        assert!(!git_dir.join("modules").exists(), "{}", git_dir.display());
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
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "fix"]);
    support::write(&wt, "a.txt", "fix\n");
    ws.git(&wt, &["commit", "-q", "-am", "fix"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&wt, &["rebase", "-q", "main"]);
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    assert!(git_dir.join("rebase-merge").is_dir());
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-fix"), "{list}");
    ws.assert_track(&app, "main", "[ahead 1]");

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: path(&git_dir),
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
fn a_linked_primary_git_does_not_list_is_not_its_own_worktree() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let main_wt = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    let git_dir = ws.add_worktree(&main_wt, &app, &["-b", "work"]);
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
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
            head: None,
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
    // worktree git dir that claims the path goes wrong in one of them
    for copy_name in ["aaa", "zzz"] {
        copied_git_dir_serves_one_worktree(copy_name, "b2");
    }
    // on the same branch: the two records are alike, and each worktree git dir
    // still serves one
    copied_git_dir_serves_one_worktree("aaa", "b1");
}

fn copied_git_dir_serves_one_worktree(copy_name: &str, copy_head: &str) {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.git(&app, &["branch", "-q", "b2", "main"]);
    let wt = ws.dir("app-wt");
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "b1"]);
    // a copy of its git dir, claiming the same path, on another branch
    let copy = git_dir.with_file_name(copy_name);
    copy_dir(&ws, &git_dir, &copy);
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
        Some(Head::Branch {
            name: copy_head.into()
        }),
        "{copy_name}"
    );
    assert!(
        matches!(&u.why, UnprobedWhy::Failed { error }
            if error.contains("doesn't point at this repo's git dir")),
        "{:?}",
        u.why
    );
}
