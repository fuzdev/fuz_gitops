//! What removing a gone worktree or its git dir would lose, and when cleanup
//! is withheld.

mod support;

use std::path::Path;

use fuz_repos::classify::{NeedsHuman, Refresh};
use fuz_repos::probe::RegistryDirs;
use fuz_repos::sessions::LiveSessions;
use fuz_repos::state::{
    CleanupReason, GitDirHolds, InProgressOp, Prune, PruneLoss, UnprobedHead, UnprobedWhy,
    UnprobedWorktree, Verdict,
};
use fuz_repos::status::{StatusOptions, status};
use support::worktrees::{NOTHING_HELD, app, pushed_branch};
use support::{FixtureWorkspace, branch, path, unprobed_facts};

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
