//! Entry-level state from real repos: presence, `needs_human` reasons, the
//! checkout's dirt, branch and pin, third-party references, `--fetch`, and
//! the runner's guards (no writes to the git dir, no hooks or fsmonitor, no
//! parent repo claiming a dir).

mod support;

use fuz_repos::classify::{NeedsHuman, OriginFix, OriginRemote};
use fuz_repos::state::{
    Head, HeldBy, InProgressOp, Presence, Relation, SyncAction, Uncommitted, Verdict,
};
use support::{FixtureWorkspace, branch, branch_names, find_entry, owned_origin};

#[test]
fn missing_and_not_a_repo() {
    let mut ws = FixtureWorkspace::new();
    ws.declare_repo("gone", "gone", "");
    ws.declare_repo("empty", "empty", "");
    ws.declare_repo("copy", "copy", "");
    ws.declare_repo("dangling", "dangling", "");
    std::fs::create_dir(ws.dir("empty")).unwrap();
    support::write(&ws.dir("copy"), "src/lib.rs", "// copied, not cloned\n");
    // a `.git` file pointing nowhere: git's own message says why
    support::write(&ws.dir("dangling"), ".git", "gitdir: /nowhere\n");
    assert!(!ws.dir("gone").exists());
    assert!(!ws.dir("copy").join(".git").exists());
    assert!(!ws.has_ref(&ws.dir("dangling"), "HEAD"));

    let entries = ws.status();
    let gone = find_entry(&entries, "gone");
    assert_eq!(gone.presence, Presence::Missing);
    assert!(gone.needs_human.is_empty());
    assert!(gone.checkouts.is_empty() && gone.branches.is_empty());
    assert_eq!(gone.layout, None);

    for (key, detail) in [
        ("empty", "empty directory"),
        ("copy", "no .git: a copy of the files, not a clone"),
    ] {
        let e = find_entry(&entries, key);
        assert_eq!(e.presence, Presence::NotARepo, "{key}");
        assert_eq!(
            e.needs_human,
            [NeedsHuman::NotARepo {
                detail: detail.into()
            }],
            "{key}"
        );
        assert!(e.checkouts.is_empty() && e.branches.is_empty(), "{key}");
    }
    let dangling = find_entry(&entries, "dangling");
    assert_eq!(dangling.presence, Presence::NotARepo);
    let [NeedsHuman::NotARepo { detail }] = &dangling.needs_human[..] else {
        panic!("{:?}", dangling.needs_human)
    };
    assert!(
        detail.contains("not a git repository") && detail.contains("/nowhere"),
        "{detail}"
    );
}

#[test]
fn a_repo_above_the_workspace_never_claims_its_dirs() {
    let mut ws = FixtureWorkspace::new();
    // the whole fixture tree, workspace included, sits inside another repo
    ws.git(ws.base(), &["init", "-q"]);
    ws.declare_repo("empty", "empty", "");
    ws.declare_repo("copy", "copy", "");
    std::fs::create_dir(ws.dir("empty")).unwrap();
    support::write(&ws.dir("copy"), "src/lib.rs", "// copied, not cloned\n");
    // control: plain git finds the parent from inside either dir
    for dir in ["empty", "copy"] {
        assert_eq!(
            ws.git(&ws.dir(dir), &["rev-parse", "--show-toplevel"]),
            ws.base().to_str().unwrap()
        );
    }

    let entries = ws.status();
    for (key, detail) in [
        ("empty", "empty directory"),
        ("copy", "no .git: a copy of the files, not a clone"),
    ] {
        let e = find_entry(&entries, key);
        assert_eq!(e.presence, Presence::NotARepo, "{key}: {e:?}");
        assert_eq!(
            e.needs_human,
            [NeedsHuman::NotARepo {
                detail: detail.into()
            }],
            "{key}"
        );
    }
}

#[test]
fn a_local_status_writes_nothing_to_the_git_dir() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    ws.commit(&app, "local");
    ws.assert_clean(&app);
    // same content, new stat info: an index refresh would rewrite the entry
    let stale = std::time::UNIX_EPOCH + std::time::Duration::from_secs(support::CLOCK_START);
    support::set_mtime(&app.join("a.txt"), stale);
    let git_dir = app.join(".git");
    let before = support::snapshot_git_dir(&git_dir);

    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    assert!(e.checkouts[0].uncommitted.is_clean());
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&git_dir));

    // control: plain `git status` does refresh the index
    let index = std::fs::read(git_dir.join("index")).unwrap();
    ws.git(&app, &["status", "--porcelain"]);
    assert_ne!(
        std::fs::read(git_dir.join("index")).unwrap(),
        index,
        "control: plain status should rewrite the stale index"
    );
}

#[test]
fn a_clean_clone_reports_its_checkout_and_layout() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.assert_clean(&app);
    ws.assert_shallow(&app, false);
    // a clone writes no FETCH_HEAD
    assert!(!app.join(".git/FETCH_HEAD").exists());

    let e = ws.entry("app");
    assert_eq!(e.presence, Presence::Present);
    assert!(e.writable);
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert_eq!(e.stashes, 0);
    // dated by its clone's own reflog entry instead
    assert_eq!(e.fetched_at, Some(ws.clone_reflog_time(&app)));
    let layout = e.layout.as_ref().unwrap();
    assert!(!layout.shallow && !layout.sparse);
    assert_eq!(layout.partial_filter, None);
    let [checkout] = &e.checkouts[..] else {
        panic!("{:?}", e.checkouts)
    };
    assert!(checkout.primary);
    assert_eq!(checkout.path, app.to_str().unwrap());
    assert_eq!(
        checkout.head,
        Head::Branch {
            name: "main".into()
        }
    );
    assert!(checkout.uncommitted.is_clean());
    assert_eq!(checkout.in_progress, None);
    assert_eq!(
        branch(&e, "main").worktree.as_deref(),
        Some(app.to_str().unwrap())
    );
}

#[test]
fn the_uncommitted_split_counts_a_conflict() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n"), ("b.txt", "b\n")]);
    // a conflict with no operation in progress: a stash that pops onto a
    // conflicting commit
    support::write(&app, "a.txt", "stashed\n");
    ws.git(&app, &["stash", "-q"]);
    support::write(&app, "a.txt", "committed\n");
    ws.git(&app, &["commit", "-q", "-am", "conflicting"]);
    ws.git_fails(&app, &["stash", "pop", "-q"]);
    // plus one of each other kind
    support::write(&app, "staged.txt", "new\n");
    ws.git(&app, &["add", "staged.txt"]);
    support::write(&app, "b.txt", "changed\n");
    support::write(&app, "untracked.txt", "?\n");
    ws.assert_porcelain(
        &app,
        &["UU a.txt", "A  staged.txt", " M b.txt", "?? untracked.txt"],
    );
    assert!(!app.join(".git/MERGE_HEAD").exists());

    let e = ws.entry("app");
    assert_eq!(
        e.checkouts[0].uncommitted,
        Uncommitted {
            staged: 1,
            unstaged: 1,
            untracked: 1,
            conflicted: 1,
        }
    );
    assert_eq!(e.checkouts[0].in_progress, None);
    // the failed pop keeps its stash
    assert_eq!(e.stashes, 1);
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    // ahead and dirty: the push still acts
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );
}

#[test]
fn stashes_are_counted() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    for content in ["one\n", "two\n"] {
        support::write(&app, "a.txt", content);
        ws.git(&app, &["stash", "-q"]);
    }
    assert_eq!(ws.git(&app, &["stash", "list"]).lines().count(), 2);
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert_eq!(e.stashes, 2);
    assert!(e.checkouts[0].uncommitted.is_clean());
}

#[test]
fn a_rebase_stopped_midway_holds_the_entry() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    // `feat` ahead, so there's an action for the rebase to hold
    ws.git(&app, &["checkout", "-q", "-b", "feat"]);
    ws.commit(&app, "feat");
    ws.git(&app, &["push", "-q", "-u", "origin", "feat"]);
    ws.commit(&app, "feat-more");
    ws.git(&app, &["checkout", "-q", "main"]);
    // main and origin/main change a.txt differently
    let up = ws.upstream("app");
    support::write(&up, "a.txt", "upstream\n");
    ws.git(&up, &["commit", "-q", "-am", "upstream"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    support::write(&app, "a.txt", "local\n");
    ws.git(&app, &["commit", "-q", "-am", "local"]);
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git_fails(&app, &["rebase", "-q", "origin/main"]);
    assert!(app.join(".git/rebase-merge").is_dir());
    ws.assert_head(&app, None);
    ws.assert_track(&app, "feat", "[ahead 1]");
    ws.assert_porcelain(&app, &["UU a.txt"]);

    let e = ws.entry("app");
    // the rebase detached HEAD by design: no `UnexpectedDetached` beside it
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: app.to_str().unwrap().into(),
            op: InProgressOp::Rebase,
        }]
    );
    assert_eq!(e.checkouts[0].in_progress, Some(InProgressOp::Rebase));
    assert_eq!(e.checkouts[0].uncommitted.conflicted, 1);
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
}

#[test]
fn a_merge_in_progress_holds_the_entry() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    ws.git(&app, &["checkout", "-q", "-b", "feat"]);
    support::write(&app, "a.txt", "feat\n");
    ws.git(&app, &["commit", "-q", "-am", "feat"]);
    ws.git(&app, &["checkout", "-q", "main"]);
    support::write(&app, "a.txt", "main\n");
    ws.git(&app, &["commit", "-q", "-am", "main"]);
    ws.git_fails(&app, &["merge", "-q", "feat"]);
    assert!(app.join(".git/MERGE_HEAD").is_file());
    ws.assert_head(&app, Some("main"));
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.assert_porcelain(&app, &["UU a.txt"]);

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: app.to_str().unwrap().into(),
            op: InProgressOp::Merge,
        }]
    );
    assert_eq!(e.checkouts[0].in_progress, Some(InProgressOp::Merge));
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
}

#[test]
fn a_bisect_in_progress_owns_its_detached_head() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let good = ws.git(&app, &["rev-parse", "HEAD"]);
    for n in 1..=3 {
        ws.commit(&app, &format!("step-{n}"));
    }
    ws.git(&app, &["bisect", "start", "HEAD", &good]);
    assert!(app.join(".git/BISECT_LOG").is_file());
    ws.assert_head(&app, None);
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: app.to_str().unwrap().into(),
            op: InProgressOp::Bisect,
        }]
    );
    assert!(matches!(e.checkouts[0].head, Head::Detached { .. }));
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 3 },
            by: HeldBy::Entry
        }
    );
}

/// `a.txt` changed on `feat` and, differently, on `main`; returns `feat`'s
/// commit as a patch file named `name`, outside the workspace, with `main`
/// checked out.
fn conflicting_patch(
    ws: &FixtureWorkspace,
    repo: &std::path::Path,
    name: &str,
) -> std::path::PathBuf {
    ws.git(repo, &["checkout", "-q", "-b", "feat"]);
    support::write(repo, "a.txt", "feat\n");
    ws.git(repo, &["commit", "-q", "-am", "feat"]);
    let patch = ws.git_raw(repo, &["format-patch", "-1", "--stdout", "feat"]);
    let patches = ws.outside("patches");
    support::write(&patches, name, &patch);
    ws.git(repo, &["checkout", "-q", "main"]);
    support::write(repo, "a.txt", "main\n");
    ws.git(repo, &["commit", "-q", "-am", "main"]);
    patches.join(name)
}

#[test]
fn an_am_stopped_on_a_conflict_is_am_not_a_rebase() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    // detached, the same stop: am applies onto HEAD wherever it is
    let det = ws.owned_repo("det", &[("a.txt", "a\n")]);
    for (repo, name) in [(&app, "app.patch"), (&det, "det.patch")] {
        let patch = conflicting_patch(&ws, repo, name);
        if repo == &det {
            ws.git(repo, &["checkout", "-q", "--detach"]);
        }
        ws.git_fails(repo, &["am", "-q", patch.to_str().unwrap()]);
        assert!(repo.join(".git/rebase-apply/applying").is_file());
        assert!(!repo.join(".git/rebase-merge").exists());
    }
    ws.assert_head(&app, Some("main"));
    ws.assert_head(&det, None);
    ws.assert_track(&app, "main", "[ahead 1]");

    let entries = ws.status();
    let e = find_entry(&entries, "app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: app.to_str().unwrap().into(),
            op: InProgressOp::Am,
        }]
    );
    assert_eq!(e.checkouts[0].in_progress, Some(InProgressOp::Am));
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
    // am never detaches HEAD, so it doesn't explain a detached one
    let e = find_entry(&entries, "det");
    assert_eq!(
        e.needs_human,
        [
            NeedsHuman::OperationInProgress {
                checkout: det.to_str().unwrap().into(),
                op: InProgressOp::Am,
            },
            NeedsHuman::UnexpectedDetached {
                checkout: det.to_str().unwrap().into(),
            },
        ]
    );
}

#[test]
fn a_rebase_on_the_apply_backend_is_still_a_rebase() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[("a.txt", "a\n")]);
    conflicting_patch(&ws, &app, "feat.patch");
    ws.git(&app, &["checkout", "-q", "feat"]);
    ws.git_fails(&app, &["rebase", "-q", "--apply", "main"]);
    // am's directory, without am's mark
    assert!(app.join(".git/rebase-apply").is_dir());
    assert!(!app.join(".git/rebase-apply/applying").exists());
    ws.assert_head(&app, None);

    let e = ws.entry("app");
    // and it owns its detached HEAD, as any rebase does
    assert_eq!(
        e.needs_human,
        [NeedsHuman::OperationInProgress {
            checkout: app.to_str().unwrap().into(),
            op: InProgressOp::Rebase,
        }]
    );
}

#[test]
fn a_stale_rebase_head_alone_is_nothing() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // git leaves REBASE_HEAD behind after some finished rebases
    let head = ws.git(&app, &["rev-parse", "HEAD"]);
    std::fs::write(app.join(".git/REBASE_HEAD"), format!("{head}\n")).unwrap();
    assert!(!app.join(".git/rebase-merge").exists());
    assert!(!app.join(".git/rebase-apply").exists());
    ws.assert_clean(&app);

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert_eq!(e.checkouts[0].in_progress, None);
}

#[test]
fn origin_mismatch_holds_the_entry_and_names_the_url_to_set() {
    let mut ws = FixtureWorkspace::new();
    for name in ["moved", "no_origin"] {
        ws.remote(name, &[]);
        ws.declare_repo(name, name, "");
    }
    // transferred between accounts: origin still names the old one
    let moved = ws.clone_as("moved", "moved", "git@github.com:someone/moved", &[]);
    ws.commit(&moved, "local");
    ws.assert_track(&moved, "main", "[ahead 1]");
    let no_origin = ws.clone_owned("no_origin", "no_origin", &[]);
    ws.git(&no_origin, &["remote", "rename", "origin", "elsewhere"]);
    assert_eq!(ws.git(&no_origin, &["remote"]), "elsewhere");

    let entries = ws.status();
    let moved = find_entry(&entries, "moved");
    assert_eq!(
        moved.needs_human,
        [NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:someone/moved".into()
            },
            expected: owned_origin("moved"),
            fix: OriginFix::SetUrl,
        }]
    );
    assert_eq!(
        branch(moved, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );
    let no_origin = find_entry(&entries, "no_origin");
    assert!(
        no_origin.needs_human.contains(&NeedsHuman::OriginMismatch {
            origin: OriginRemote::Missing,
            expected: owned_origin("no_origin"),
            fix: OriginFix::Add,
        }),
        "{:?}",
        no_origin.needs_human
    );
}

#[test]
fn an_https_origin_matches_an_owned_entry() {
    // SSH and HTTPS forms name the same repo; only the transport differs
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.clone_as("app", "app", "https://github.com/me/app.git", &[]);

    let e = ws.entry("app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
}

#[test]
fn the_registry_branch_missing_or_without_an_upstream() {
    let mut ws = FixtureWorkspace::new();
    for name in ["other_branch", "no_upstream"] {
        ws.remote(name, &[]);
    }
    ws.declare_repo("other_branch", "other_branch", "branch = \"develop\"");
    ws.declare_repo("no_upstream", "no_upstream", "");
    let other = ws.clone_owned("other_branch", "other_branch", &[]);
    let no_upstream = ws.clone_owned("no_upstream", "no_upstream", &[]);
    ws.git(&no_upstream, &["branch", "-q", "--unset-upstream", "main"]);
    // off the registry's branch, so only that rule keeps it from cleanup
    ws.git(&no_upstream, &["checkout", "-q", "-b", "other"]);
    assert!(!ws.has_ref(&other, "develop"));
    ws.assert_upstream(&no_upstream, "main", "");
    ws.assert_count(&no_upstream, &["main", "--not", "--remotes"], 0);

    let entries = ws.status();
    assert_eq!(
        find_entry(&entries, "other_branch").needs_human,
        [NeedsHuman::DefaultBranchMissing {
            branch: "develop".into()
        }]
    );
    let no_upstream = find_entry(&entries, "no_upstream");
    assert_eq!(
        no_upstream.needs_human,
        [NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into()
        }]
    );
    // the registry's branch without an upstream isn't merged work, even
    // when it isn't checked out
    let main = branch(no_upstream, "main");
    assert_eq!(main.relation, Relation::Untracked);
    assert_eq!(main.worktree, None);
    assert_eq!(main.verdict, Verdict::Quiet);
}

#[test]
fn unexpected_detached() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["checkout", "-q", "--detach"]);
    ws.assert_head(&app, None);

    let e = ws.entry("app");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::UnexpectedDetached {
            checkout: app.to_str().unwrap().into()
        }]
    );
    assert!(matches!(e.checkouts[0].head, Head::Detached { .. }));
    let main = branch(&e, "main");
    assert_eq!(main.worktree, None);
    assert_eq!(main.verdict, Verdict::Quiet);
}

#[test]
fn pinned_detached_holds_a_stale_main() {
    let mut ws = FixtureWorkspace::new();
    for name in ["lib", "fork"] {
        ws.remote(name, &[]);
    }
    ws.declare_reference("lib", support::THIRD_PARTY, "lib", "pinned = true");
    ws.declare_reference("fork", support::OWNER, "fork", "pinned = true");
    let lib = ws.clone_third_party("lib", "lib", &[]);
    let fork = ws.clone_owned("fork", "fork", &[]);
    for (name, repo) in [("lib", &lib), ("fork", &fork)] {
        // detached at the pinned commit; local main left behind a moved origin
        ws.git(repo, &["checkout", "-q", "--detach"]);
        ws.upstream_commit(name, "main");
        ws.git(repo, &["fetch", "-q", "origin"]);
        ws.assert_head(repo, None);
        ws.assert_track(repo, "main", "[behind 1]");
    }

    let entries = ws.status();
    let lib = find_entry(&entries, "lib");
    assert!(!lib.writable);
    assert_eq!((lib.branch.as_deref(), lib.pinned), (None, true));
    assert!(lib.needs_human.is_empty(), "{:?}", lib.needs_human);
    // third-party: only branches with local work are kept
    assert!(lib.branches.is_empty(), "{:?}", lib.branches);
    // owned and pinned: never moved, the pin naming the hold
    let fork = find_entry(&entries, "fork");
    assert!(fork.writable);
    assert!(fork.needs_human.is_empty(), "{:?}", fork.needs_human);
    let main = branch(fork, "main");
    assert_eq!(main.relation, Relation::Behind { commits: 1 });
    assert_eq!(
        main.verdict,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::Pinned,
        }
    );
}

#[test]
fn pinned_on_its_branch_reads_clean() {
    let mut ws = FixtureWorkspace::new();
    for name in ["lib", "fork"] {
        ws.remote(name, &[]);
    }
    ws.upstream_commit("fork", "pin");
    ws.declare_reference("lib", support::THIRD_PARTY, "lib", "pinned = true");
    ws.declare_reference(
        "fork",
        support::OWNER,
        "fork",
        "branch = \"pin\"\npinned = true",
    );
    let lib = ws.clone_third_party("lib", "lib", &[]);
    let fork = ws.clone_owned("fork", "fork", &["--branch", "pin"]);
    ws.assert_head(&lib, Some("main"));
    ws.assert_head(&fork, Some("pin"));
    // the fork's pin sits behind the branch it lives on
    ws.upstream_commit("fork", "pin");
    ws.git(&fork, &["fetch", "-q", "origin"]);
    ws.assert_track(&fork, "pin", "[behind 1]");

    let entries = ws.status();
    let lib = find_entry(&entries, "lib");
    assert!(lib.needs_human.is_empty(), "{:?}", lib.needs_human);
    assert!(lib.branches.is_empty(), "{:?}", lib.branches);
    let fork = find_entry(&entries, "fork");
    assert_eq!((fork.branch.as_deref(), fork.pinned), (Some("pin"), true));
    assert!(fork.needs_human.is_empty(), "{:?}", fork.needs_human);
    let pin = branch(fork, "pin");
    assert_eq!(pin.relation, Relation::Behind { commits: 1 });
    let held = Verdict::Held {
        action: SyncAction::FastForward { commits: 1 },
        by: HeldBy::Pinned,
    };
    assert_eq!(pin.verdict, held);

    // `--fetch` passes the pin over: the branch it lives on moves again
    // upstream, unseen
    let fork_dir = ws.dir("fork");
    let tracking = ws.git(&fork_dir, &["rev-parse", "refs/remotes/origin/pin"]);
    let fetch_head = std::fs::read(fork_dir.join(".git/FETCH_HEAD")).unwrap();
    let moved = ws.upstream_commit("fork", "pin");
    assert_ne!(moved, tracking);
    let entries = ws.status_with_fetch();
    let fork = find_entry(&entries, "fork");
    assert_eq!(fork.fetch_error, None);
    assert_eq!(
        ws.git(&fork_dir, &["rev-parse", "refs/remotes/origin/pin"]),
        tracking
    );
    assert_eq!(
        std::fs::read(fork_dir.join(".git/FETCH_HEAD")).unwrap(),
        fetch_head
    );
    ws.assert_track(&fork_dir, "pin", "[behind 1]");
    assert!(fork.needs_human.is_empty(), "{:?}", fork.needs_human);
    assert_eq!(branch(fork, "pin").verdict, held);
}

#[test]
fn a_pin_whose_upstream_is_gone_is_not_offered_for_cleanup() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("fork", &[]);
    // both at main, whose commits stay on a remote
    for b in ["pin", "side"] {
        ws.git(
            &ws.upstream("fork"),
            &["push", "-q", "origin", &format!("main:{b}")],
        );
    }
    ws.declare_reference(
        "fork",
        support::OWNER,
        "fork",
        "branch = \"pin\"\npinned = true",
    );
    let fork = ws.clone_owned("fork", "fork", &["--branch", "pin"]);
    ws.git(&fork, &["branch", "-q", "--track", "side", "origin/side"]);
    // the pin carries local work; then both are deleted upstream
    ws.commit(&fork, "local");
    for b in ["pin", "side"] {
        ws.upstream_delete_branch("fork", b);
    }
    ws.git(&fork, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&fork, "pin", "[gone]");
    ws.assert_track(&fork, "side", "[gone]");

    let e = ws.entry("fork");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    // the branch the pinned commit lives on is local work, never cleanup
    let pin = branch(&e, "pin");
    assert_eq!(pin.relation, Relation::Gone);
    assert_eq!(pin.unique_commits, 1);
    assert_eq!(pin.verdict, Verdict::LocalOnly);
    // nothing unique beside it: nothing to say
    let side = branch(&e, "side");
    assert_eq!(side.relation, Relation::Gone);
    assert_eq!(side.unique_commits, 0);
    assert_eq!(side.verdict, Verdict::Quiet);
}

#[test]
fn a_pin_branch_ahead_on_remote_commits_is_quiet() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("fork", &[]);
    ws.upstream_commit("fork", "pin");
    ws.upstream_commit("fork", "next");
    ws.declare_reference(
        "fork",
        support::OWNER,
        "fork",
        "branch = \"pin\"\npinned = true",
    );
    let fork = ws.clone_owned("fork", "fork", &["--branch", "pin"]);
    // main fast-forwarded from another remote's ref, as a fork's main
    // from its upstream: ahead of origin/main with nothing unique
    ws.git(&fork, &["branch", "-q", "--track", "main", "origin/main"]);
    ws.git(&fork, &["update-ref", "refs/heads/main", "origin/next"]);
    ws.assert_track(&fork, "main", "[ahead 1]");
    // the pin, ahead with a commit on no remote
    ws.commit(&fork, "local");
    ws.assert_track(&fork, "pin", "[ahead 1]");

    let e = ws.entry("fork");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let main = branch(&e, "main");
    assert_eq!(main.relation, Relation::Ahead { commits: 1 });
    assert_eq!(main.unique_commits, 0);
    assert_eq!(main.verdict, Verdict::Quiet);
    let pin = branch(&e, "pin");
    assert_eq!(pin.relation, Relation::Ahead { commits: 1 });
    assert_eq!(pin.unique_commits, 1);
    assert_eq!(pin.verdict, Verdict::LocalOnly);
}

#[test]
fn a_third_party_reference_keeps_only_local_only_work() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_reference("lib", support::THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party("lib", "lib", &[]);
    ws.git(&lib, &["checkout", "-q", "-b", "audit"]);
    ws.commit(&lib, "audit-1");
    let tip = ws.commit(&lib, "audit-2");
    // plus a branch behind its moved origin, which third-party entries
    // never compare
    ws.git(&lib, &["checkout", "-q", "main"]);
    ws.upstream_commit("lib", "main");
    ws.git(&lib, &["fetch", "-q", "origin"]);
    ws.assert_track(&lib, "main", "[behind 1]");
    ws.assert_count(&lib, &["audit", "--not", "--remotes"], 2);

    let e = ws.entry("lib");
    assert!(!e.writable);
    assert_eq!((e.branch.as_deref(), e.pinned), (None, false));
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert_eq!(branch_names(&e), ["audit"]);
    let audit = branch(&e, "audit");
    assert_eq!(audit.relation, Relation::Untracked);
    assert_eq!(audit.unique_commits, 2);
    assert_eq!(audit.newest_commit_at, ws.committer_time(&lib, &tip));
    assert_eq!(audit.verdict, Verdict::LocalOnly);
}

#[test]
fn tracked_hooks_and_an_fsmonitor_never_run() {
    let mut ws = FixtureWorkspace::new();
    let hook_marker = ws.outside("hook-ran");
    let fsmonitor_marker = ws.outside("fsmonitor-ran");
    let marker_script =
        |marker: &std::path::Path| format!("#!/bin/sh\necho \"$0\" >> '{}'\n", marker.display());
    ws.remote("app", &[]);
    // the hooks dir is tracked
    let up = ws.upstream("app");
    for hook in [
        "reference-transaction",
        "post-index-change",
        "post-checkout",
        "post-merge",
        "pre-auto-gc",
    ] {
        support::write_executable(&up, &format!("hooks/{hook}"), &marker_script(&hook_marker));
    }
    ws.git(&up, &["add", "hooks"]);
    ws.git(&up, &["commit", "-q", "-m", "hooks"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    assert!(app.join("hooks/reference-transaction").is_file());
    // an fsmonitor hook that fails, so git falls back to a full scan
    let fsmonitor = ws.outside("fsmonitor.sh");
    support::write_executable(
        ws.base(),
        "fsmonitor.sh",
        &format!("{}exit 1\n", marker_script(&fsmonitor_marker)),
    );
    // something for --fetch to update, and dirt for status to look at
    ws.upstream_commit("app", "main");
    support::write(&app, "README", "touched\n");
    ws.assert_porcelain(&app, &[" M README"]);

    // last, so no other setup call runs them: point the checkout at both,
    // and show plain git runs them — a ref update fires
    // `reference-transaction`, a status fires the fsmonitor
    ws.git(&app, &["config", "core.hooksPath", "hooks"]);
    // git runs it through the shell: quoted, so a tempdir with spaces holds
    // (one with a `'` breaks this and the marker scripts, loudly, in setup)
    let fsmonitor = format!("'{}'", fsmonitor.display());
    ws.git(&app, &["config", "core.fsmonitor", &fsmonitor]);
    ws.git(&app, &["update-ref", "refs/fixture/control", "HEAD"]);
    ws.git(&app, &["status", "--porcelain"]);
    assert!(hook_marker.exists(), "control: plain git runs the hooks");
    assert!(
        fsmonitor_marker.exists(),
        "control: plain git runs the fsmonitor"
    );
    std::fs::remove_file(&hook_marker).unwrap();
    std::fs::remove_file(&fsmonitor_marker).unwrap();

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    assert_eq!(e.probe_error, None);
    assert_eq!(
        branch(&e, "main").relation,
        Relation::Behind { commits: 1 },
        "the fetch ran"
    );
    assert_eq!(e.checkouts[0].uncommitted.unstaged, 1);
    for marker in [&hook_marker, &fsmonitor_marker] {
        assert!(
            !marker.exists(),
            "{} ran: {}",
            marker.display(),
            std::fs::read_to_string(marker).unwrap_or_default()
        );
    }
}

#[test]
fn fetch_updates_remote_tracking_refs_and_fetched_at() {
    let mut ws = FixtureWorkspace::new();
    for name in ["app", "lib", "fork"] {
        ws.remote(name, &[]);
    }
    ws.declare_repo("app", "app", "");
    ws.declare_reference("lib", support::THIRD_PARTY, "lib", "");
    ws.declare_reference("fork", support::OWNER, "fork", "pinned = true");
    let app = ws.clone_owned("app", "app", &[]);
    let lib = ws.clone_third_party("lib", "lib", &[]);
    let fork = ws.clone_owned("fork", "fork", &[]);
    ws.git(&fork, &["checkout", "-q", "--detach"]);
    // a new branch and a moved main upstream
    ws.upstream_commit("app", "feat");
    for name in ["app", "lib", "fork"] {
        ws.upstream_commit(name, "main");
    }
    let before = ws.git(&app, &["rev-parse", "main"]);
    for repo in [&app, &lib, &fork] {
        ws.assert_track(repo, "main", "");
        assert!(!repo.join(".git/FETCH_HEAD").exists());
    }

    // local refs only: nothing moved yet
    let entries = ws.status();
    let e = find_entry(&entries, "app");
    let cloned_at = ws.clone_reflog_time(&app);
    assert_eq!(e.fetched_at, Some(cloned_at));
    assert_eq!(branch(e, "main").relation, Relation::InSync);

    let entries = ws.status_with_fetch();
    let e = find_entry(&entries, "app");
    assert_eq!(e.fetch_error, None);
    // `FETCH_HEAD`'s mtime now: the machine's clock, past the fixture's
    assert!(e.fetched_at.is_some_and(|at| at > cloned_at));
    assert_eq!(branch(e, "main").relation, Relation::Behind { commits: 1 });
    // remote-tracking refs only: the local branch stays put
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), before);
    ws.assert_track(&app, "main", "[behind 1]");
    assert!(ws.has_ref(&app, "refs/remotes/origin/feat"));
    ws.assert_clean(&app);
    // third-party and pinned entries aren't fetched
    for (key, repo) in [("lib", &lib), ("fork", &fork)] {
        let e = find_entry(&entries, key);
        assert_eq!(e.fetched_at, Some(ws.clone_reflog_time(repo)), "{key}");
        assert!(!repo.join(".git/FETCH_HEAD").exists(), "{key}");
        ws.assert_track(repo, "main", "");
    }
}

#[test]
fn fetch_prunes_a_deleted_upstream_to_gone() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.upstream_delete_branch("app", "feat");
    // the local view still has it until a pruning fetch
    ws.assert_track(&app, "feat", "");
    assert!(ws.has_ref(&app, "refs/remotes/origin/feat"));

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    assert!(!ws.has_ref(&app, "refs/remotes/origin/feat"));
    let feat = branch(&e, "feat");
    assert_eq!(feat.relation, Relation::Gone);
    assert_eq!(
        feat.verdict,
        Verdict::Cleanup {
            reason: fuz_repos::state::CleanupReason::UpstreamGone,
            removable_worktree: None
        }
    );
}

#[test]
fn fetch_never_runs_auto_maintenance() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // a second pack trips auto-gc, in the foreground
    for (key, value) in [
        ("gc.autoPackLimit", "1"),
        ("fetch.unpackLimit", "1"),
        ("gc.autoDetach", "false"),
        ("maintenance.autoDetach", "false"),
    ] {
        ws.git(&app, &["config", key, value]);
    }
    let packs = || {
        std::fs::read_dir(app.join(".git/objects/pack"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "pack")
            })
            .count()
    };
    assert_eq!(packs(), 1);
    ws.upstream_commit("app", "main");

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::Behind { commits: 1 });
    assert_eq!(packs(), 2, "auto maintenance repacked after the fetch");

    // control: a plain fetch runs it, collapsing the packs
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    assert_eq!(packs(), 1, "control: plain fetch should auto-gc");
}
