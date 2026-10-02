//! `repos push` rebasing the diverged registry branch it was asked to push:
//! its local-only commits replayed onto the fetched tip, the branch and the
//! target's checkout moved there, and the replayed tip pushed — through
//! sync's own rebase and push — and every way it's held, refused, or left
//! to a person, each followed by the exact refs, index, and working tree
//! both sides should hold (nothing else moved).
//!
//! The live-sessions reader is the seam, as in `sync_rebase.rs`: its first
//! call classifies, its second is the one right before the rebase, its
//! third the one right before the push that follows.
//!
//! Upstream history a test builds by hand reaches the bare remote by a
//! fetch into it.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use fuz_repos::push::PushRun;
use fuz_repos::report::{BranchSyncHold, NoUpstreamWhy, PushOutcome, RebasePush, RebaseRefusal};
use fuz_repos::sessions::{LiveSessions, SessionSource};
use fuz_repos::state::{BranchHold, BranchNeedsHuman, Relation, SyncAction, Verdict};
use fuz_repos::{PUSH_FORMAT_VERSION, STATUS_FORMAT_VERSION};
use support::busy::{claude_dir, live_session, read_as};
use support::cli::{REPOS, parse, repos, stderr, stdout};
use support::push::{only, pushes_served, remote_refs, with};
use support::rebase::{
    ORIGIN_HEAD, TRACKING, assert_replayed, diverge, diverged, publish, untouched,
};
use support::{
    FixtureWorkspace, LiveChild, branch, files, find_entry, git_env, quiet, reader_then, write,
    write_executable,
};

const fn rebase(ahead: u32, behind: u32) -> SyncAction {
    SyncAction::Rebase { ahead, behind }
}

/// The one target's `Rebased` outcome: `(from, to, onto, push)`.
fn rebased(run: &PushRun) -> (&str, &str, &str, &RebasePush) {
    match only(run) {
        (
            Some("main"),
            PushOutcome::Rebased {
                from,
                to,
                onto,
                push,
            },
        ) => (from, to, onto, push),
        other => panic!("not rebased: {other:?}"),
    }
}

const fn held(by: BranchSyncHold) -> PushOutcome {
    PushOutcome::Held { by }
}

/// `repos push --json`'s report, whatever the run's exit (`parse` takes
/// only a run that exited `0`).
fn report(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).unwrap()
}

/// `repos push` in `cwd`, wide enough that a hint stays on its line.
fn push_wide(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> Output {
    ws.command(REPOS, cwd)
        .env("COLUMNS", "400")
        .arg("push")
        .args(args)
        .output()
        .unwrap()
}

const DIVERGED_HINT: &str = "              hint: repos push rebases the registry's branch onto \
     origin's when its commits replay cleanly, then pushes it, and never force-pushes; any \
     other diverged branch is resolved by hand";

const REBASED_HINT: &str = "              hint: a rebase replays the branch's commits onto \
     origin's as new commits and moves the checkout to them: commit ids from before it are \
     stale, and anything checked before it was checked on the old base";

const DIRTY_REBASE_HINT: &str = "              hint: a diverged branch is rebased before it's \
     pushed, which needs a clean checkout (untracked files count): commit, or git stash -u, \
     then repos push again";

// --- rebased, then pushed ---

#[test]
fn a_diverged_registry_branch_is_rebased_then_pushed() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);

    // the verdict the push acted on is sync's own
    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").relation,
        Relation::Diverged {
            ahead: 2,
            behind: 1
        }
    );
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Act {
            action: rebase(2, 1)
        }
    );
    // the report carries the tips: the one replaced, the new one, and the
    // fetched tip it was replayed onto
    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert!(run.pushes[0].outcome.in_sync());
    assert_replayed(&ws, &d, to);
    // the branch, and the remote-tracking ref the push recorded: nothing
    // else
    assert_eq!(
        ws.refs(app),
        with(
            &before,
            &[("refs/heads/main", to), (TRACKING, to), (ORIGIN_HEAD, to)]
        )
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", to)])
    );
    assert_eq!(pushes_served(&ws).len(), 1);
    // the checkout followed: on the branch at the new tip, clean, both
    // sides' files
    ws.assert_head(app, Some("main"));
    assert_eq!(ws.git(app, &["rev-parse", "HEAD"]), to);
    ws.assert_clean(app);
    assert_eq!(
        files(app),
        ["README", "local-1.txt", "local-2.txt", "upstream-main.txt"]
    );
    ws.assert_track(app, "main", "");
    // nothing left to do
    let again = ws.push(&["app"]);
    assert_eq!(only(&again), (Some("main"), &PushOutcome::InSync));
    assert_eq!(pushes_served(&ws).len(), 1);
}

#[test]
fn new_branch_rebases_a_diverged_branch_as_a_push_without_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);

    let run = ws.push_new_branch(&["app"]);

    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    ws.assert_clean(&d.app);
}

// --- a dirty checkout holds it ---

#[test]
fn a_dirty_checkout_holds_the_rebase_untracked_files_too() {
    for dirt in ["untracked", "unstaged", "staged"] {
        let mut ws = FixtureWorkspace::new();
        let d = diverged(&mut ws);
        let app = &d.app;
        match dirt {
            "untracked" => write(app, "scratch.txt", "mine\n"),
            "unstaged" => write(app, "README", "edited\n"),
            _ => {
                write(app, "README", "edited\n");
                ws.git(app, &["add", "README"]);
            }
        }
        assert_ne!(ws.git_raw(app, &["status", "--porcelain"]), "", "{dirt}");
        let before = untouched(&ws, app);

        let run = ws.push(&["app"]);

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
            only(&run),
            (Some("main"), &held(BranchSyncHold::DirtyCheckout)),
            "{dirt}"
        );
        assert!(!run.pushes[0].outcome.in_sync(), "{dirt}");
        // nothing moved, nothing replayed onto, nothing sent
        assert_eq!(untouched(&ws, app), before, "{dirt}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{dirt}");
    }
}

#[test]
fn a_held_rebase_exits_one_and_names_the_fix() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // an untracked file beside a tracked edit: a plain `git stash` would
    // leave the first, and the push held
    write(app, "scratch.txt", "mine\n");
    write(app, "README", "edited\n");
    ws.assert_porcelain(app, &[" M README", "?? scratch.txt"]);
    let before = untouched(&ws, app);

    let out = push_wide(&ws, app, &[]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        ["held          app +2 −1 (dirty)", DIRTY_REBASE_HINT],
        "{text}"
    );
    assert_eq!(untouched(&ws, app), before);
    let doc = report(&repos(&ws, app, &["push", "--json"]));
    assert_eq!(
        (&doc["pushes"][0]["kind"], &doc["pushes"][0]["by"]),
        (&"held".into(), &"dirty_checkout".into())
    );
    assert_eq!(untouched(&ws, app), before);

    // the fix run as the hint prints it: stashed with its untracked file,
    // the same command rebases and pushes, and the stash goes back on top
    ws.git(app, &["stash", "-u"]);
    ws.assert_clean(app);
    let out = push_wide(&ws, app, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_replayed(&ws, &d, &to);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    ws.git(app, &["stash", "pop", "-q"]);
    ws.assert_porcelain(app, &[" M README", "?? scratch.txt"]);
    assert_eq!(
        std::fs::read_to_string(app.join("scratch.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        std::fs::read_to_string(app.join("README")).unwrap(),
        "edited\n"
    );
}

#[test]
fn a_file_appearing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = ws.refs(app);
    // the very path the upstream's commit adds, untracked here
    let scratch = app.join("upstream-main.txt");
    let read = reader_then(2, || std::fs::write(&scratch, "mine\n").unwrap());

    let run = ws.push_with(&["app"], &ws.root(), &read);

    // clean when classified: the rebase's own re-check found it
    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Act {
            action: rebase(2, 1)
        }
    );
    assert_eq!(
        only(&run),
        (Some("main"), &held(BranchSyncHold::DirtyCheckout))
    );
    assert_eq!(ws.refs(app), before);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), "mine\n");
    ws.assert_porcelain(app, &["?? upstream-main.txt"]);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- a conflict moves nothing ---

#[test]
fn a_conflict_moves_nothing_and_exits_one() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // both sides add the same path, each its own content
    let local = ws.commit(&app, "upstream-main");
    let upstream = ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    assert_ne!(
        ws.git(&app, &["rev-parse", "main:upstream-main.txt"]),
        ws.git(&app, &["rev-parse", "origin/main:upstream-main.txt"])
    );
    ws.assert_clean(&app);
    ws.write_registry();
    let before = untouched(&ws, &app);

    let out = repos(&ws, &app, &["push", "--json"]);

    // sync exits 0 on a conflict, a designed stop; a push that didn't land
    // exits 1, as any does
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let doc = report(&out);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
            "kind": "rebase_refused",
            "why": {"kind": "conflicts"},
        }])
    );
    // the branch, the index, the files, and the remote: byte for byte
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
    assert_eq!(ws.git(&app, &["rev-parse", TRACKING]), upstream);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    // no rebase left in progress
    assert!(!app.join(".git/rebase-merge").exists());
    assert!(!app.join(".git/REBASE_HEAD").exists());

    let out = push_wide(&ws, &app, &[]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "needs human   app (diverged +1 −1, rebase conflicts)",
            DIVERGED_HINT
        ],
        "{text}"
    );
    assert_eq!(untouched(&ws, &app), before);
}

#[test]
fn a_local_commit_already_upstream_is_refused_not_kept_empty() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    // the same change, made here too (a cherry-pick's shape)
    let content = std::fs::read_to_string(ws.upstream("app").join("upstream-main.txt")).unwrap();
    write(&app, "upstream-main.txt", &content);
    ws.git(&app, &["add", "-A"]);
    ws.git(&app, &["commit", "-q", "-m", "the same change"]);
    let same = ws.git(&app, &["rev-parse", "HEAD"]);
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    assert_eq!(
        ws.git(&app, &["rev-parse", "main:upstream-main.txt"]),
        ws.git(&app, &["rev-parse", "origin/main:upstream-main.txt"])
    );
    ws.assert_clean(&app);
    ws.write_registry();
    let before = untouched(&ws, &app);

    let run = ws.push(&["app"]);

    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::RebaseRefused {
                why: RebaseRefusal::AlreadyUpstream { commit: same }
            }
        )
    );
    assert!(!run.pushes[0].outcome.in_sync());
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- a diverged branch that isn't the tool's to rebase ---

/// `app` with a merge among `main`'s local-only commits.
fn merge_in_range(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    ws.commit(&app, "topic");
    ws.git(&app, &["switch", "-q", "main"]);
    ws.commit(&app, "local");
    ws.git(
        &app,
        &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
    );
    ws.git(&app, &["branch", "-q", "-D", "topic"]);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 3, behind 1]");
    ws.assert_count(&app, &["--merges", "origin/main..main"], 1);
    app
}

/// `app` with a tag on one of `main`'s local-only commits.
fn tag_in_range(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    let d = diverge(ws, app);
    ws.git(&d.app, &["tag", "v1", &d.local[0]]);
    assert_eq!(ws.git(&d.app, &["rev-parse", "v1^{commit}"]), d.local[0]);
    d.app
}

/// `app` whose `main` is ahead by a commit origin's `feat` holds, and one
/// of its own.
fn published_in_range(ws: &mut FixtureWorkspace) -> PathBuf {
    ws.remote("app", &[]);
    let feat = ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["merge", "-q", "--ff-only", "origin/feat"]);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), feat);
    ws.commit(&app, "local");
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 2, behind 1]");
    ws.assert_count(&app, &["main", "--not", "--remotes"], 1);
    app
}

/// `app`, archived, its `main` diverged.
fn archived(ws: &mut FixtureWorkspace) -> PathBuf {
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "archived = true");
    let app = ws.clone_owned("app", "app", &[]);
    diverge(ws, app).app
}

#[test]
fn a_diverged_branch_sync_wouldnt_rebase_is_a_persons_by_its_reason() {
    type Setup = fn(&mut FixtureWorkspace) -> PathBuf;
    let cases: [(&str, Setup, BranchNeedsHuman); 4] = [
        ("a merge", merge_in_range, BranchNeedsHuman::DivergedMerge),
        ("a tag", tag_in_range, BranchNeedsHuman::DivergedTagged),
        (
            "a published commit",
            published_in_range,
            BranchNeedsHuman::DivergedPublished,
        ),
        ("archived", archived, BranchNeedsHuman::Diverged),
    ];
    for (case, setup, reason) in cases {
        let mut ws = FixtureWorkspace::new();
        let app = setup(&mut ws);
        ws.assert_head(&app, Some("main"));
        ws.assert_clean(&app);
        ws.write_registry();
        let before = untouched(&ws, &app);

        let run = ws.push(&["app"]);

        let e = find_entry(&run.entries, "app");
        assert_eq!(
            branch(e, "main").verdict,
            Verdict::NeedsHuman { reason },
            "{case}"
        );
        assert_eq!(
            only(&run),
            (Some("main"), &PushOutcome::NeedsHuman { reason }),
            "{case}"
        );
        assert!(!run.pushes[0].outcome.in_sync(), "{case}");
        assert_eq!(untouched(&ws, &app), before, "{case}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_reason_is_said_with_the_hint_and_exits_one() {
    let mut ws = FixtureWorkspace::new();
    let app = merge_in_range(&mut ws);
    ws.write_registry();

    let out = push_wide(&ws, &app, &[]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "needs human   app (diverged +3 −1, a merge among its commits)",
            DIVERGED_HINT
        ],
        "{text}"
    );
}

// --- what holds a push holds the rebase ---

#[test]
fn a_busy_checkout_holds_the_rebase() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![live_session(&child, app, SessionSource::SessionFile)]);

    // another session in the checkout when classified
    let run = ws.push_with(&["app"], &ws.root(), &|| live.clone());
    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: rebase(2, 1),
            by: BranchHold::Busy
        }
    );
    assert_eq!(only(&run), (Some("main"), &held(BranchSyncHold::Busy)));
    assert_eq!(untouched(&ws, app), before);

    // one arriving after the fetch holds it too, re-read right before
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
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
            action: rebase(2, 1)
        }
    );
    assert_eq!(only(&run), (Some("main"), &held(BranchSyncHold::Busy)));
    assert_eq!(untouched(&ws, app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn the_callers_own_session_holds_nothing() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    // a session recorded in the checkout, as Claude Code records one
    let claude = claude_dir(&ws, "claude");
    let session = LiveChild::spawn();
    claude.session(session.pid(), &session.proc_start(), app);

    // read by anyone else, it holds the rebase
    let run = ws.push_with(&["app"], &ws.root(), &|| read_as(&claude, &session, false));
    assert_eq!(only(&run), (Some("main"), &held(BranchSyncHold::Busy)));
    assert_eq!(untouched(&ws, app), before);

    // read by the session itself — the one running `repos push` — it's
    // excluded, at classifying and at each re-check
    let run = ws.push_with(&["app"], &ws.root(), &|| read_as(&claude, &session, true));
    let (from, to, _, push) = rebased(&run);
    assert_eq!(from, d.tip());
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
}

#[test]
fn origin_drift_and_a_failed_fetch_hold_the_rebase() {
    type Unverified = fn(&FixtureWorkspace, &Path);
    let cases: [(&str, Unverified, BranchSyncHold); 2] = [
        (
            "push URL",
            |ws, app| {
                ws.git(
                    app,
                    &["config", "remote.origin.pushurl", "git@github.com:me/other"],
                );
            },
            BranchSyncHold::PushUrl,
        ),
        (
            "fetch failed",
            |ws, _| std::fs::remove_dir_all(ws.bare("app")).unwrap(),
            BranchSyncHold::FetchFailed,
        ),
    ];
    for (case, unverify, by) in cases {
        let mut ws = FixtureWorkspace::new();
        let d = diverged(&mut ws);
        let app = &d.app;
        unverify(&ws, app);
        let before = ws.refs(app);

        let run = ws.push(&["app"]);

        assert_eq!(only(&run), (Some("main"), &held(by)), "{case}");
        assert_eq!(ws.refs(app), before, "{case}");
        ws.assert_clean(app);
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_commit_landing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let remote_before = remote_refs(&ws, "app");
    let landed = std::sync::Mutex::new(String::new());
    // another hand commits on the branch between classifying and acting
    let read = reader_then(2, || {
        git_env(
            &env,
            app,
            &["commit", "-q", "--allow-empty", "-m", "meanwhile"],
        );
        *landed.lock().unwrap() = git_env(&env, app, &["rev-parse", "HEAD"]);
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    assert_eq!(only(&run), (Some("main"), &held(BranchSyncHold::Changed)));
    // the commit that landed is the branch's tip still: never replayed
    // around, never dropped
    let landed = landed.lock().unwrap().clone();
    assert_eq!(ws.git(app, &["rev-parse", "main"]), landed);
    assert_eq!(ws.git(app, &["rev-parse", "main~1"]), d.tip());
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), d.upstream);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

// --- the target is a checkout: a linked worktree's, and by key or path ---

#[test]
fn a_linked_worktree_target_is_rebased_in_its_own_checkout() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // the primary moves to a branch of its own, with no upstream; `main`
    // lives in a linked worktree outside the workspace
    ws.git(app, &["switch", "-q", "-c", "side", &d.upstream]);
    let side = ws.commit(app, "side");
    ws.assert_upstream(app, "side", "");
    let wt = ws.outside("app-main");
    ws.add_worktree(app, &wt, &["main"]);
    ws.assert_head(&wt, Some("main"));
    ws.assert_clean(&wt);
    ws.assert_track(app, "main", "[ahead 2, behind 1]");
    let primary_files = files(app);
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");

    // by key: the entry's own checkout, the primary, on `side` — and no
    // other branch is touched, the diverged `main` beside it included
    let run = ws.push(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("side"),
            &PushOutcome::NoUpstream {
                why: NoUpstreamWhy::Creatable
            }
        )
    );
    assert_eq!(run.pushes[0].checkout, app.to_str().unwrap());
    assert_eq!(ws.refs(app), before);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_track(app, "main", "[ahead 2, behind 1]");
    assert_eq!(pushes_served(&ws), Vec::<String>::new());

    // by path: the worktree's own checkout, on `main`
    let run = ws.push(&[wt.to_str().unwrap()]);
    assert_eq!(run.pushes[0].checkout, wt.to_str().unwrap());
    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    assert_eq!(
        ws.refs(app),
        with(
            &before,
            &[("refs/heads/main", to), (TRACKING, to), (ORIGIN_HEAD, to)]
        )
    );
    // the worktree followed its branch
    ws.assert_head(&wt, Some("main"));
    assert_eq!(ws.git(&wt, &["rev-parse", "HEAD"]), to);
    ws.assert_clean(&wt);
    assert_eq!(
        files(&wt),
        ["README", "local-1.txt", "local-2.txt", "upstream-main.txt"]
    );
    // the primary, on another branch, wasn't touched
    ws.assert_head(app, Some("side"));
    assert_eq!(ws.git(app, &["rev-parse", "HEAD"]), side);
    ws.assert_clean(app);
    assert_eq!(files(app), primary_files);
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", to)])
    );
}

#[test]
fn a_dirty_linked_worktree_holds_its_own_rebase() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    ws.git(app, &["switch", "-q", "--detach", &d.upstream]);
    let wt = ws.outside("app-main");
    ws.add_worktree(app, &wt, &["main"]);
    // the dirt is the target's: the primary is clean
    write(&wt, "scratch.txt", "mine\n");
    ws.assert_porcelain(&wt, &["?? scratch.txt"]);
    ws.assert_clean(app);
    let before = ws.refs(app);

    let run = ws.push_from(&[], &wt);

    assert_eq!(
        only(&run),
        (Some("main"), &held(BranchSyncHold::DirtyCheckout))
    );
    assert_eq!(ws.refs(app), before);
    ws.assert_porcelain(&wt, &["?? scratch.txt"]);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- the push that follows ---

#[test]
fn a_push_the_remote_refuses_leaves_the_branch_rebased_for_the_rerun() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let hook = ws.bare("app").join("hooks/pre-receive");
    write_executable(
        &ws.bare("app"),
        "hooks/pre-receive",
        "#!/bin/sh\necho 'error: GH006: Protected branch update failed.' >&2\nexit 1\n",
    );
    let remote_before = remote_refs(&ws, "app");

    let out = repos(&ws, app, &["push", "--json"]);

    // rebased, not pushed: the branch isn't in sync with its upstream
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let doc = report(&out);
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
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
        }])
    );
    assert_replayed(&ws, &d, &to);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), d.upstream);
    ws.assert_track(app, "main", "[ahead 2]");
    ws.assert_clean(app);

    // refused again, the rerun says so in text: a branch ahead, no rebase
    let out = push_wide(&ws, app, &[]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "failed        app (push: rejected: GH006: Protected branch update failed.)\n"
        ),
        "{}",
        stdout(&out)
    );
    assert_eq!(ws.git(app, &["rev-parse", "main"]), to);

    // the refusal lifted, the rerun pushes it
    std::fs::remove_file(hook).unwrap();
    let out = push_wide(&ws, app, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("pushed        app +2\n"),
        "{}",
        stdout(&out)
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    ws.assert_track(app, "main", "");
}

#[test]
fn a_remote_that_moved_after_the_fetch_fails_the_lease_of_a_rebased_branch() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let bare = ws.bare("app");
    let up = ws.upstream("app");
    // a commit the upstream author holds, on origin's tip, not yet there
    let later = ws.commit(&up, "later");
    assert_eq!(ws.git(&up, &["rev-parse", "HEAD~1"]), d.upstream);
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), d.upstream);
    let up_path = up.to_str().unwrap().to_owned();
    // it lands on origin after the rebase, right before the push
    let read = reader_then(3, || {
        git_env(
            &env,
            &bare,
            &["fetch", "-q", &up_path, "+refs/heads/main:refs/heads/main"],
        );
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    // rebased onto the tip the fetch saw; the lease refused the push
    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(
        push,
        &RebasePush::Held {
            by: BranchSyncHold::Changed
        }
    );
    assert!(!run.pushes[0].outcome.in_sync());
    assert_replayed(&ws, &d, to);
    // reached, and left where the other hand put it: never forced over
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), later);
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), d.upstream);
    assert_eq!(pushes_served(&ws).len(), 1);
    ws.assert_clean(app);
    ws.assert_track(app, "main", "[ahead 2]");

    // the rerun fetches the commit, finds the branch diverged again, and
    // rebases it once more
    let to = to.to_owned();
    let rerun = ws.push(&["app"]);
    let (from_again, to_again, onto_again, push) = rebased(&rerun);
    assert_eq!((from_again, onto_again), (to.as_str(), later.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), to_again);
    assert_eq!(ws.git(app, &["rev-parse", "main~2"]), later);
    ws.assert_track(app, "main", "");
    ws.assert_clean(app);
}

#[test]
fn a_replayed_tip_another_hand_pushed_meanwhile_reads_in_sync() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let bare = ws.bare("app");
    let app_path = app.to_str().unwrap().to_owned();
    let before = ws.refs(app);
    let found = std::sync::Mutex::new(Vec::new());
    // the replayed tip itself reaches origin after the rebase, right before
    // the push, by a fetch into the remote
    let read = reader_then(3, || {
        git_env(
            &env,
            &bare,
            &["fetch", "-q", &app_path, "+refs/heads/main:refs/heads/main"],
        );
        *found.lock().unwrap() = vec![
            git_env(&env, &bare, &["rev-parse", "main"]),
            git_env(&env, app, &["rev-parse", "main"]),
            git_env(&env, app, &["rev-parse", TRACKING]),
        ];
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    // rebased, and nothing left to send: in sync, so the run exits 0
    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::AlreadyThere);
    assert!(run.pushes[0].outcome.in_sync());
    assert_replayed(&ws, &d, to);
    // as the push found it: origin at the replayed tip, the remote-tracking
    // ref still at the fetched one
    assert_eq!(
        *found.lock().unwrap(),
        [to.to_owned(), to.to_owned(), d.upstream.clone()]
    );
    // the remote, the branch, and the remote-tracking ref the push recorded
    // all hold the replayed tip
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), to);
    assert_eq!(
        ws.refs(app),
        with(
            &before,
            &[("refs/heads/main", to), (TRACKING, to), (ORIGIN_HEAD, to)]
        )
    );
    // the remote was asked, under the lease, and had it already
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/app'"]);
    ws.assert_clean(app);
    ws.assert_track(app, "main", "");
    let rerun = ws.push(&["app"]);
    assert_eq!(only(&rerun), (Some("main"), &PushOutcome::InSync));
    assert_eq!(pushes_served(&ws).len(), 1);
}

#[test]
fn a_rebase_never_replaces_an_ignored_file_and_names_it() {
    // upstream starts tracking a path the clone ignores and holds locally:
    // status reads the checkout clean, and git's switch refuses to
    // overwrite it
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[(".gitignore", "secret.env\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    let local = ws.commit(&app, "local");
    let up = ws.upstream("app");
    write(&up, "secret.env", "tracked\n");
    ws.git(&up, &["add", "-f", "secret.env"]);
    ws.git(&up, &["commit", "-q", "-m", "track it"]);
    publish(&ws, "app");
    write(&app, "secret.env", "mine\n");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    ws.assert_clean(&app);
    ws.write_registry();
    let before = untouched(&ws, &app);
    let said = "error: The following untracked working tree files would be overwritten by \
                checkout: secret.env";

    let out = repos(&ws, &app, &["push", "--json"]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let doc = report(&out);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
            "kind": "failed",
            "message": said,
        }])
    );
    let out = push_wide(&ws, &app, &[]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(&format!("failed        app (rebase: {said})\n")),
        "{}",
        stdout(&out)
    );
    // the replay ran, and nothing moved: the branch, the index, the file
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
    assert_eq!(
        std::fs::read_to_string(app.join("secret.env")).unwrap(),
        "mine\n"
    );
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
}

#[test]
fn a_session_arriving_before_the_push_holds_it_rebased() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![live_session(&child, app, SessionSource::SessionFile)]);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    // none when classifying or rebasing; one in the checkout right before
    // the push
    let read = || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            quiet()
        } else {
            live.clone()
        }
    };

    let run = ws.push_with(&["app"], &ws.root(), &read);

    let (from, to, _, push) = rebased(&run);
    assert_eq!(from, d.tip());
    assert_eq!(
        push,
        &RebasePush::Held {
            by: BranchSyncHold::Busy
        }
    );
    assert_replayed(&ws, &d, to);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_track(app, "main", "[ahead 2]");
}

// --- through the binary: several targets, the text, and the document ---

#[test]
fn an_agent_pushes_one_target_and_rebases_another() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let blog = ws.owned_repo("blog", &[]);
    let blog_tip = ws.commit(&blog, "local");
    ws.assert_track(&blog, "main", "[ahead 1]");
    // a third, diverged too, that no target names: sync's, not this run's
    let site = ws.owned_repo("site", &[]);
    let site_tip = ws.commit(&site, "local");
    ws.upstream_commit("site", "main");
    ws.git(&site, &["fetch", "-q", "origin"]);
    ws.assert_track(&site, "main", "[ahead 1, behind 1]");
    ws.write_registry();
    let site_remote = remote_refs(&ws, "site");

    // an agent's push rebases and pushes as a person's does
    let out = ws
        .command(REPOS, &ws.root())
        .env("CLAUDECODE", "1")
        .env("COLUMNS", "400")
        .args(["push", "app", "blog"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_replayed(&ws, &d, &to);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            format!(
                "rebased       app +2 (onto 1 new upstream commit, now {}, was {})",
                &to[..7],
                &d.tip()[..7]
            )
            .as_str(),
            REBASED_HINT,
            "pushed        app +2  blog +1",
        ],
        "{text}"
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    assert_eq!(ws.git(&ws.bare("blog"), &["rev-parse", "main"]), blog_tip);
    ws.assert_clean(app);
    // the entry no target named: as it was, on both sides
    assert_eq!(ws.git(&site, &["rev-parse", "main"]), site_tip);
    assert_eq!(remote_refs(&ws, "site"), site_remote);
}

#[test]
fn push_json_carries_the_rebase_and_its_tips() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;

    let out = repos(&ws, app, &["push", "--json"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let doc = parse(&out);
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    assert_eq!(doc["status"]["version"], STATUS_FORMAT_VERSION);
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
            "kind": "rebased",
            "from": d.tip(),
            "to": to,
            "onto": d.upstream,
            "push": {"kind": "pushed"},
        }])
    );
    // the counts are the branch's relation, in the state the push acted on
    let main = &doc["status"]["entries"][0]["branches"][0];
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
    assert_replayed(&ws, &d, &to);
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
}

/// Dirt matters only when the push must rebase: a branch ahead pushes
/// from a dirty checkout, and the same dirt holds it once it diverged.
#[test]
fn the_same_dirt_holds_only_a_branch_that_diverged() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let first = ws.commit(&app, "local-1");
    write(&app, "README", "edited\n");
    write(&app, "scratch.txt", "mine\n");
    ws.assert_porcelain(&app, &[" M README", "?? scratch.txt"]);
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();

    // ahead and dirty: pushed, the files as they were
    let run = ws.push(&["app"]);
    assert!(
        matches!(only(&run), (Some("main"), PushOutcome::Pushed { to, .. }) if *to == first),
        "{:?}",
        run.pushes
    );
    ws.assert_porcelain(&app, &[" M README", "?? scratch.txt"]);

    // diverged, the same dirt: held
    ws.git(&app, &["commit", "-q", "--allow-empty", "-m", "local-2"]);
    // the upstream author takes what was pushed, then commits on it
    let up = ws.upstream("app");
    ws.git(&up, &["pull", "-q", "--ff-only", "origin", "main"]);
    assert_eq!(ws.git(&up, &["rev-parse", "main"]), first);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    ws.assert_porcelain(&app, &[" M README", "?? scratch.txt"]);
    let before = untouched(&ws, &app);

    let run = ws.push(&["app"]);

    assert_eq!(
        only(&run),
        (Some("main"), &held(BranchSyncHold::DirtyCheckout))
    );
    assert_eq!(untouched(&ws, &app), before);
}
