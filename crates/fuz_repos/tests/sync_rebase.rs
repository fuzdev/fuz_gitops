//! `repos sync` rebasing a diverged registry branch: its local-only commits
//! replayed onto the fetched tip, the branch moved there — in its clean
//! checkout, a linked worktree's, or in place — and pushed; which branches
//! it rebases, and which it leaves to a person; the conflicts the replay
//! refuses; what holds it as the verdict says; and what the binary says of
//! it. Each run is followed by the refs, index, and working tree both sides
//! should hold.
//!
//! The holds found only at the moment of acting are `sync_rebase_races`',
//! and what the replay writes and the move refuses, `sync_rebase_replay`'s.
//! The fixtures and what a run must leave are `support::rebase`'s, shared
//! with those files and the `push_rebase*` tests.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::{BranchOutcome, BranchSyncHold, RebasePush, RebaseRefusal};
use fuz_repos::state::{BranchHold, BranchNeedsHuman, Relation, Verdict};
use fuz_repos::{STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use support::busy::live_in;
use support::cli::{REPOS, parse, repos, stderr, stdout};
use support::push::{pushes_served, remote_refs, with};
use support::rebase::{
    DIRT, REBASED_FILES, archived, assert_replayed, conflicting, dirty, diverge, diverged,
    merge_in_range, published_in_range, rebase, rebase_held, refs_rebased, refuse_pushes,
    sync_rebased, tag_in_range, untouched,
};
use support::sync::outcome;
use support::{
    FixtureWorkspace, LiveChild, OWNER, TRACKING, assert_git_dir_unchanged, branch, files,
    find_entry, snapshot_git_dir, write,
};

// --- rebased, then pushed ---

#[test]
fn a_diverged_registry_branch_is_rebased_in_its_checkout_and_pushed() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");

    // status previews it, from the facts alone
    let e = ws.entry("app");
    let main = branch(&e, "main");
    assert_eq!(
        main.relation,
        Relation::Diverged {
            ahead: 2,
            behind: 1
        }
    );
    assert_eq!(
        main.verdict,
        Verdict::Act {
            action: rebase(2, 1)
        }
    );
    assert_eq!(ws.refs(app), before);

    let run = ws.sync();

    let (from, to, onto, push) = sync_rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert!(!outcome(&run, "app", "main").failed());
    assert_replayed(&ws, &d, to);
    assert_eq!(ws.refs(app), refs_rebased(&before, to));
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", to)])
    );
    assert_eq!(pushes_served(&ws).len(), 1);
    // the checkout followed: on the branch, clean, both sides' files
    ws.assert_head(app, Some("main"));
    ws.assert_clean(app);
    assert_eq!(files(app), REBASED_FILES);
    ws.assert_track(app, "main", "");
    // nothing left to do
    let again = ws.sync();
    assert_eq!(outcome(&again, "app", "main"), &BranchOutcome::Untouched);
}

#[test]
fn a_diverged_registry_branch_no_checkout_has_is_rebased_in_place() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // the checkout moves to a branch of its own, a file of its own in it
    ws.git(app, &["switch", "-q", "-c", "side", &d.upstream]);
    let side = ws.commit(app, "side");
    ws.assert_head(app, Some("side"));
    assert_eq!(
        ws.git(
            app,
            &[
                "for-each-ref",
                "--format=%(worktreepath)",
                "refs/heads/main"
            ]
        ),
        ""
    );
    ws.assert_track(app, "main", "[ahead 2, behind 1]");
    let before = ws.refs(app);
    let tree_before = files(app);

    let run = ws.sync();

    let (from, to, onto, push) = sync_rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    assert_eq!(ws.refs(app), refs_rebased(&before, to));
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    // the checkout, on another branch, wasn't touched
    ws.assert_head(app, Some("side"));
    assert_eq!(ws.git(app, &["rev-parse", "HEAD"]), side);
    ws.assert_clean(app);
    assert_eq!(files(app), tree_before);
}

#[test]
fn a_branch_a_linked_worktree_has_is_rebased_there() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // the primary moves to a branch of its own; `main` lives in a linked
    // worktree
    ws.git(app, &["switch", "-q", "-c", "side", &d.upstream]);
    let side = ws.commit(app, "side");
    let wt = ws.outside("app-main");
    ws.add_worktree(app, &wt, &["main"]);
    ws.assert_head(&wt, Some("main"));
    ws.assert_clean(&wt);
    ws.assert_track(app, "main", "[ahead 2, behind 1]");
    let primary_files = files(app);

    let run = ws.sync();

    let (from, to, onto, push) = sync_rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    // the worktree followed its branch
    ws.assert_head(&wt, Some("main"));
    assert_eq!(ws.git(&wt, &["rev-parse", "HEAD"]), *to);
    ws.assert_clean(&wt);
    assert_eq!(files(&wt), REBASED_FILES);
    // the primary, on another branch, wasn't touched
    ws.assert_head(app, Some("side"));
    assert_eq!(ws.git(app, &["rev-parse", "HEAD"]), side);
    ws.assert_clean(app);
    assert_eq!(files(app), primary_files);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), *to);
}

#[test]
fn a_commit_empty_to_begin_with_is_replayed_as_it_was() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["commit", "-q", "--allow-empty", "-m", "a marker"]);
    let marker = ws.git(&app, &["rev-parse", "HEAD"]);
    assert_eq!(
        ws.git(&app, &["rev-parse", "HEAD^{tree}"]),
        ws.git(&app, &["rev-parse", "HEAD~1^{tree}"])
    );
    let upstream = ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    ws.write_registry();

    let run = ws.sync();

    let (from, to, onto, push) = sync_rebased(&run);
    assert_eq!((from, onto), (marker.as_str(), upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), to);
    assert_eq!(
        ws.git(&app, &["log", "-1", "--format=%s", "main"]),
        "a marker"
    );
    assert_eq!(
        ws.git(&app, &["rev-parse", "main^{tree}"]),
        ws.git(&app, &["rev-parse", &format!("{upstream}^{{tree}}")])
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
}

// --- what the replay refuses: a person's, nothing moved ---

#[test]
fn a_conflict_moves_nothing_and_leaves_it_to_a_person() {
    let mut ws = FixtureWorkspace::new();
    let c = conflicting(&mut ws, "app");
    let app = &c.app;
    // status can't know: it predicts a rebase, and never replays
    assert_eq!(
        branch(&ws.entry("app"), "main").verdict,
        Verdict::Act {
            action: rebase(1, 1)
        }
    );
    let before = untouched(&ws, app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::RebaseRefused {
            why: RebaseRefusal::Conflicts
        }
    );
    // a person's call, not a failure: the run exits as it does for any
    // diverged branch
    assert!(!outcome(&run, "app", "main").failed());
    assert_eq!(untouched(&ws, app), before);
    assert_eq!(ws.git(app, &["rev-parse", "main"]), c.local);
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), c.upstream);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_head(app, Some("main"));
    ws.assert_clean(app);
    // no operation left in progress, as the tool reads one
    assert_eq!(ws.entry("app").checkouts[0].in_progress, None);
    ws.assert_track(app, "main", "[ahead 1, behind 1]");
}

#[test]
fn a_local_commit_already_upstream_is_refused_not_kept_empty() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let upstream = ws.upstream_commit("app", "main");
    // the same change, made here too (a cherry-pick's shape), between two
    // commits of its own
    let content = std::fs::read_to_string(ws.upstream("app").join("upstream-main.txt")).unwrap();
    ws.commit(&app, "local-1");
    write(&app, "upstream-main.txt", &content);
    ws.git(&app, &["add", "-A"]);
    ws.git(&app, &["commit", "-q", "-m", "the same change"]);
    let same = ws.git(&app, &["rev-parse", "HEAD"]);
    ws.commit(&app, "local-2");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 3, behind 1]");
    assert_eq!(
        ws.git(&app, &["rev-parse", &format!("{same}:upstream-main.txt")]),
        ws.git(&app, &["rev-parse", "origin/main:upstream-main.txt"])
    );
    ws.assert_clean(&app);
    ws.write_registry();
    let before = untouched(&ws, &app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::RebaseRefused {
            why: RebaseRefusal::AlreadyUpstream { commit: same }
        }
    );
    assert!(!outcome(&run, "app", "main").failed());
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(ws.git(&app, &["rev-parse", TRACKING]), upstream);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_track(&app, "main", "[ahead 3, behind 1]");
}

// --- a diverged branch that isn't the tool's to rebase ---

#[test]
fn a_merge_among_the_local_commits_is_a_persons() {
    let mut ws = FixtureWorkspace::new();
    let app = merge_in_range(&mut ws);
    let reason = BranchNeedsHuman::DivergedMerge;
    assert_eq!(
        branch(&ws.entry("app"), "main").verdict,
        Verdict::NeedsHuman { reason }
    );
    let before = untouched(&ws, &app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman { reason }
    );
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_tag_on_a_local_commit_is_a_persons() {
    for annotated in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let d = tag_in_range(&mut ws, annotated);
        let app = &d.app;
        let reason = BranchNeedsHuman::DivergedTagged;
        assert_eq!(
            branch(&ws.entry("app"), "main").verdict,
            Verdict::NeedsHuman { reason },
            "annotated: {annotated}"
        );
        let before = untouched(&ws, app);

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &BranchOutcome::NeedsHuman { reason },
            "annotated: {annotated}"
        );
        assert_eq!(untouched(&ws, app), before, "annotated: {annotated}");
        assert_eq!(
            pushes_served(&ws),
            Vec::<String>::new(),
            "annotated: {annotated}"
        );

        // the tag gone, sync rebases it
        ws.git(app, &["tag", "-d", "v1"]);
        let run = ws.sync();
        let (_, to, _, push) = sync_rebased(&run);
        assert_eq!(push, &RebasePush::Pushed, "annotated: {annotated}");
        assert_replayed(&ws, &d, to);
    }
}

#[test]
fn a_commit_another_remote_branch_holds_is_never_rewritten() {
    for own_commit_too in [false, true] {
        let case = format!("own commit too: {own_commit_too}");
        let mut ws = FixtureWorkspace::new();
        let (app, feat) = published_in_range(&mut ws, own_commit_too);
        let reason = BranchNeedsHuman::DivergedPublished;
        let e = ws.entry("app");
        assert_eq!(
            branch(&e, "main").unique_commits,
            u32::from(own_commit_too),
            "{case}"
        );
        assert_eq!(
            branch(&e, "main").verdict,
            Verdict::NeedsHuman { reason },
            "{case}"
        );
        let before = untouched(&ws, &app);

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &BranchOutcome::NeedsHuman { reason },
            "{case}"
        );
        assert_eq!(untouched(&ws, &app), before, "{case}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
        // origin's `feat` still holds the very commit `main` does
        assert!(
            ws.git_output(&app, &["merge-base", "--is-ancestor", &feat, "main"])
                .status
                .success(),
            "{case}"
        );
    }
}

#[test]
fn a_diverged_feature_branch_stays_a_persons() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.commit(&app, "local");
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "feat", "[ahead 1, behind 1]");
    ws.assert_clean(&app);
    ws.write_registry();
    let before = untouched(&ws, &app);

    let run = ws.sync();

    let reason = BranchNeedsHuman::Diverged;
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "feat").verdict, Verdict::NeedsHuman { reason });
    assert_eq!(
        outcome(&run, "app", "feat"),
        &BranchOutcome::NeedsHuman { reason }
    );
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn an_archived_repos_diverged_branch_stays_a_persons() {
    let mut ws = FixtureWorkspace::new();
    let d = archived(&mut ws);
    let before = untouched(&ws, &d.app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(untouched(&ws, &d.app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_pins_diverged_branch_is_left_alone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_reference("app", OWNER, "app", "branch = \"main\"\npinned = true");
    let app = ws.clone_owned("app", "app", &[]);
    let d = diverge(&ws, app);
    ws.write_registry();
    let before = untouched(&ws, &d.app);

    let run = ws.sync();

    // a pin's refs are stale by contract: its commits read as local work
    let e = find_entry(&run.entries, "app");
    assert!(e.pinned);
    assert_eq!(branch(e, "main").verdict, Verdict::LocalOnly);
    assert_eq!(outcome(&run, "app", "main"), &BranchOutcome::Untouched);
    assert_eq!(untouched(&ws, &d.app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_shallow_clones_branch_with_local_work_is_never_rebased() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--depth", "1"]);
    ws.assert_shallow(&app, true);
    ws.commit(&app, "local");
    ws.upstream_commit("app", "main");
    ws.assert_clean(&app);
    ws.write_registry();
    let local = ws.git(&app, &["rev-parse", "main"]);
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    let reason = BranchNeedsHuman::ShallowLocalWork;
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").relation, Relation::Shallow);
    assert_eq!(branch(e, "main").verdict, Verdict::NeedsHuman { reason });
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman { reason }
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(&app);
}

// --- held as the verdict says ---

#[test]
fn a_dirty_checkout_holds_the_rebase_untracked_files_too() {
    for dirt in DIRT {
        let mut ws = FixtureWorkspace::new();
        let d = diverged(&mut ws);
        let app = &d.app;
        dirty(&ws, app, dirt);
        let before = untouched(&ws, app);

        let run = ws.sync();

        let e = find_entry(&run.entries, "app");
        assert_eq!(
            branch(e, "main").verdict,
            Verdict::Held {
                action: rebase(2, 1),
                by: BranchHold::DirtyCheckout
            },
            "{dirt}"
        );
        assert_eq!(
            outcome(&run, "app", "main"),
            &rebase_held(BranchSyncHold::DirtyCheckout),
            "{dirt}"
        );
        assert_eq!(untouched(&ws, app), before, "{dirt}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{dirt}");
    }
}

#[test]
fn a_busy_checkout_holds_the_rebase() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let child = LiveChild::spawn();
    let live = live_in(&child, app);

    let run = ws.sync_with(4, &|| live.clone());

    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: rebase(2, 1),
            by: BranchHold::Busy
        }
    );
    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::Busy)
    );
    assert_eq!(untouched(&ws, app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_push_url_elsewhere_holds_the_registry_branchs_rebase() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    ws.git(
        &d.app,
        &["config", "remote.origin.pushurl", "git@github.com:me/other"],
    );
    // the registry's branch, diverged, would be pushed: the entry's reason,
    // and the rebase held for it
    let e = ws.entry("app");
    assert!(
        e.needs_human
            .iter()
            .any(|r| matches!(r, NeedsHuman::PushUrlMismatch { .. })),
        "{:?}",
        e.needs_human
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: rebase(2, 1),
            by: BranchHold::PushUrl
        }
    );
    let before = untouched(&ws, &d.app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::PushUrl)
    );
    assert_eq!(untouched(&ws, &d.app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- the push that follows ---

#[test]
fn a_push_the_remote_refuses_leaves_the_branch_rebased_for_the_rerun() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let hook = refuse_pushes(&ws, "app");
    let remote_before = remote_refs(&ws, "app");

    let out = repos(&ws, &ws.root(), &["sync", "--json"]);

    // a failed push fails the run, rebased or not
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let report: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_eq!(
        report["entries"][0]["branches"][0],
        serde_json::json!({
            "name": "main",
            "kind": "rebased",
            "from": d.tip(),
            "to": to,
            "onto": d.upstream,
            "push": {
                "kind": "push_failed",
                "failure": {
                    "kind": "rejected",
                    "reason": "pre-receive hook declined",
                    "message": "GH006: Protected branch update failed.",
                },
            },
            "repeats": null,
        })
    );
    assert_replayed(&ws, &d, &to);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), d.upstream);
    ws.assert_track(app, "main", "[ahead 2]");
    ws.assert_clean(app);

    // the refusal lifted, the rerun pushes it: a branch ahead, no rebase
    std::fs::remove_file(hook).unwrap();
    let rerun = ws.sync();
    assert_eq!(
        outcome(&rerun, "app", "main"),
        &BranchOutcome::Pushed {
            from: d.upstream.clone(),
            to: to.clone(),
        }
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
}

// --- through the binary: the text and the documents ---

#[test]
fn status_previews_the_rebase_and_an_agents_sync_makes_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let git_dir = snapshot_git_dir(&app.join(".git"));

    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with("sync would    rebase app +2 −1\n"),
        "{text}"
    );
    let text = stdout(&repos(&ws, &ws.root(), &["status", "--verbose", "app"]));
    // the branch's line, its age aside (the fixture's clock against now)
    assert!(
        text.lines().map(str::trim).any(|l| {
            l.starts_with("branch    main  origin/main  diverged +2 −1 · 2 unique · ")
                && l.ends_with(" · checked out → rebase")
        }),
        "{text}"
    );
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(report["version"], STATUS_FORMAT_VERSION);
    let main = &report["entries"][0]["branches"][0];
    assert_eq!(
        main["relation"],
        serde_json::json!({"kind": "diverged", "ahead": 2, "behind": 1})
    );
    assert_eq!(
        main["verdict"],
        serde_json::json!({
            "kind": "act",
            "action": {"kind": "rebase", "ahead": 2, "behind": 1},
        })
    );
    // a prediction: status replayed nothing — no object written — and
    // moved nothing
    assert_eq!(untouched(&ws, app), before);
    assert_git_dir_unchanged(&git_dir, &snapshot_git_dir(&app.join(".git")));

    // an agent's sync rebases and pushes as a person's does
    let out = ws
        .command(REPOS, &ws.root())
        .env("CLAUDECODE", "1")
        .args(["sync", "--json"])
        .output()
        .unwrap();
    let report = parse(&out);
    assert_eq!(report["version"], SYNC_FORMAT_VERSION);
    assert_eq!(report["status"]["version"], STATUS_FORMAT_VERSION);
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_eq!(
        report["entries"][0]["branches"][0],
        serde_json::json!({
            "name": "main",
            "kind": "rebased",
            "from": d.tip(),
            "to": to,
            "onto": d.upstream,
            "push": {"kind": "pushed"},
            "repeats": null,
        })
    );
    assert_replayed(&ws, &d, &to);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(text.starts_with("clean 1 · on branches 0"), "{text}");
}

#[test]
fn sync_text_says_a_rebase_and_a_conflict_and_exits_zero() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    // `blog`: both sides add the same path
    let blog = conflicting(&mut ws, "blog");

    let out = repos(&ws, &ws.root(), &["sync"]);

    // a conflict is a person's call, as any diverged branch: no failure
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "needs human   blog (diverged +1 −1, rebase conflicts)",
            "synced        rebase app +2 −1",
        ],
        "{text}"
    );
    assert_eq!(ws.git(&blog.app, &["rev-parse", "main"]), blog.local);
    ws.assert_clean(&blog.app);
    ws.assert_track(&d.app, "main", "");
    // the conflict stands; status says what sync would try again
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with("sync would    rebase blog +1 −1\n"),
        "{text}"
    );
}
