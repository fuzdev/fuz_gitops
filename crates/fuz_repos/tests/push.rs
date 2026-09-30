//! `repos push` over fixture workspaces: the branch checked out at each
//! target pushed to its upstream — through sync's own push, the exact
//! commit classified, nothing else — every way it's held or left alone,
//! the races the lease and the direct URL close, and the binary's exit
//! codes and documents. Each run is followed by the exact refs it should
//! leave on both sides.
//!
//! Pushes reach the local bare remotes over the fixture's own `ssh`, which
//! serves the registry's SSH URLs (the support module says how). The
//! live-sessions reader is the seam for what happens between classifying
//! and pushing: its second call is the one right before the push.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::PUSH_FORMAT_VERSION;
use fuz_repos::push::PushRun;
use fuz_repos::report::{FetchOutcome, PushOutcome, SyncHold};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchNeedsHuman, HeldBy, Relation, SyncAction, Verdict};
use serde_json::Value;
use support::{
    FixtureWorkspace, LiveChild, THIRD_PARTY, branch, find_entry, owned_origin, write_executable,
};

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

const fn quiet() -> LiveSessions {
    LiveSessions::Known(Vec::new())
}

/// The one target's outcome, and the branch it names.
fn only(run: &PushRun) -> (Option<&str>, &PushOutcome) {
    assert_eq!(run.pushes.len(), 1, "{:?}", run.pushes);
    let p = &run.pushes[0];
    (p.branch.as_deref(), &p.outcome)
}

fn pushed(from: &str, to: &str) -> PushOutcome {
    PushOutcome::Pushed {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// Runs git in `dir` under the fixture's environment `env` (a `Sync`
/// stand-in for `FixtureWorkspace::git` inside a reader), asserting success.
fn git_env(env: &[(OsString, OsString)], dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A reader that finds no session, and on its call number `at` (the first
/// is the one after the fetches, the second right before the push) first
/// runs `then`.
fn reader_then(at: usize, then: impl Fn() + Sync) -> impl Fn() -> LiveSessions + Sync {
    let calls = AtomicUsize::new(0);
    move || {
        if calls.fetch_add(1, Ordering::SeqCst) + 1 == at {
            then();
        }
        quiet()
    }
}

/// The remote's refs: every ref of `name`'s bare remote, tags included, and
/// its `HEAD`.
fn remote_refs(ws: &FixtureWorkspace, name: &str) -> BTreeMap<String, String> {
    ws.refs(&ws.bare(name))
}

/// `before` with `changes` applied.
fn with(before: &BTreeMap<String, String>, changes: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut refs = before.clone();
    for (r, oid) in changes {
        refs.insert((*r).to_owned(), (*oid).to_owned());
    }
    refs
}

/// The receive-pack calls the fixture's `ssh` served, by the command the
/// host ran.
fn pushes_served(ws: &FixtureWorkspace) -> Vec<String> {
    ws.ssh_log()
        .iter()
        .filter_map(|l| l.rsplit_once(" git@github.com ").map(|(_, c)| c.to_owned()))
        .filter(|c| c.starts_with("git-receive-pack"))
        .collect()
}

/// `app`, owned, its `main` ahead of origin by one commit; returns the
/// clone and that commit.
fn ahead(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    let app = ws.owned_repo("app", &[]);
    let tip = ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    (app, tip)
}

/// `app` with `feat` on its remote, cloned, `feat` checked out tracking
/// origin's and a commit ahead of it; returns the clone and that commit.
fn feat_ahead(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.assert_upstream(&app, "feat", "refs/remotes/origin/feat");
    let tip = ws.commit(&app, "local");
    ws.assert_track(&app, "feat", "[ahead 1]");
    ws.write_registry();
    (app, tip)
}

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
    let run = ws.push_with(&[], &wt, &quiet);
    assert_eq!(only(&run), (Some("feat"), &pushed(&origin_feat, &feat_tip)));
    assert_eq!(run.pushes[0].checkout, wt.to_str().unwrap());
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/feat", &feat_tip)])
    );
    // a key, a dir name, and a path in the primary, the cwd a subdir: each
    // names the primary, pushed once and in sync after
    let run = ws.push_with(&["app", "app-dir", "."], &app.join("sub"), &quiet);
    assert_eq!(only(&run), (Some("main"), &pushed(&origin_main, &main_tip)));
    assert_eq!(run.pushes[0].checkout, app.to_str().unwrap());
    let run = ws.push_with(&[], &app.join("sub"), &quiet);
    assert_eq!(only(&run), (Some("main"), &PushOutcome::InSync));
    // the worktree by its path, beside the primary by key
    let run = ws.push_with(&["app", wt.to_str().unwrap()], &ws.root(), &quiet);
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

#[test]
fn a_diverged_branch_is_a_persons() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    ws.upstream_commit("app", "main");
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push(&["app"]);

    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Diverged
            }
        )
    );
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
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
    assert_eq!(only(&run), (Some("topic"), &PushOutcome::NoUpstream));
    assert_eq!(remote_refs(&ws, "app"), remote_before);

    // an upstream deleted on origin: the fetch prunes it, and it stays gone
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.upstream_delete_branch("app", "feat");
    let remote_before = remote_refs(&ws, "app");
    assert!(!remote_before.contains_key("refs/heads/feat"));
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("feat"), &PushOutcome::NoUpstream));
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
        (Some("main"), &PushOutcome::Held { by: SyncHold::Busy })
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
        (Some("main"), &PushOutcome::Held { by: SyncHold::Busy })
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn origin_drift_holds_the_push() {
    // (config set in the clone, the hold its verdict names)
    type Drift = fn(&FixtureWorkspace, &Path);
    let cases: [(&str, Drift, SyncHold); 2] = [
        (
            "origin",
            |ws, app| ws.set_origin(app, "other", "git@github.com:me/other"),
            SyncHold::Entry,
        ),
        (
            "pushurl",
            |ws, app| {
                ws.git(
                    app,
                    &["config", "remote.origin.pushurl", "git@github.com:me/other"],
                );
            },
            SyncHold::PushUrl,
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
    assert_eq!(only(&run), (Some("topic"), &PushOutcome::NoUpstream));

    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::Held {
                by: SyncHold::FetchFailed
            }
        )
    );
    ws.assert_upstream(&app, "topic", "");
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- --new-branch ---

/// `app` with a local `topic` checked out, no upstream, a commit on it;
/// returns the clone and that commit.
fn topic(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    let tip = ws.commit(&app, "topic");
    ws.assert_upstream(&app, "topic", "");
    ws.write_registry();
    (app, tip)
}

/// Asserts `topic`'s upstream is origin's `topic` as `git push -u` sets it,
/// tracking the commit `tip`, in sync.
fn assert_tracks_origin(ws: &FixtureWorkspace, app: &Path, branch: &str, tip: &str) {
    let key = |k: &str| format!("branch.{branch}.{k}");
    assert_eq!(
        ws.git(app, &["config", "--get-all", &key("remote")]),
        "origin"
    );
    assert_eq!(
        ws.git(app, &["config", "--get-all", &key("merge")]),
        format!("refs/heads/{branch}")
    );
    ws.assert_upstream(app, branch, &format!("refs/remotes/origin/{branch}"));
    assert_eq!(
        ws.git(
            app,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        tip
    );
    ws.assert_track(app, branch, "");
}

#[test]
fn new_branch_creates_a_branch_with_no_upstream_and_tracks_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    ws.git(&app, &["tag", "v1", &tip]);
    ws.git(&app, &["config", "push.followTags", "true"]);
    let remote_before = remote_refs(&ws, "app");
    let local_before = ws.refs(&app);

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(
        only(&run),
        (Some("topic"), &PushOutcome::Created { to: tip.clone() })
    );
    assert!(run.pushes[0].outcome.in_sync());
    // the one ref, under its own name, no tag
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/topic", &tip)])
    );
    assert_eq!(
        ws.refs(&app),
        with(&local_before, &[("refs/remotes/origin/topic", &tip)])
    );
    assert_tracks_origin(&ws, &app, "topic", &tip);
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/app'"]);
    // from local refs alone, it reads in sync; and a push has nothing to do
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "topic").relation, Relation::InSync);
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("topic"), &PushOutcome::InSync));
    // then kept pushed as any branch with an upstream
    let next = ws.commit(&app, "next");
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("topic"), &pushed(&tip, &next)));
}

#[test]
fn new_branch_recreates_a_same_named_upstream_gone_from_origin() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = feat_ahead(&mut ws);
    ws.upstream_delete_branch("app", "feat");
    let config_before = ws.git(&app, &["config", "--get-regexp", "^branch\\."]);

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(
        only(&run),
        (Some("feat"), &PushOutcome::Created { to: tip.clone() })
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "feat"]), tip);
    assert_tracks_origin(&ws, &app, "feat", &tip);
    // its upstream was set already: left as it was
    assert_eq!(
        ws.git(&app, &["config", "--get-regexp", "^branch\\."]),
        config_before
    );
}

/// Merged into origin's default branch and deleted there, as GitHub does
/// with a merged PR's branch: nothing of it on no remote, so nothing to put
/// back — it reads as it does without the flag, and recreating it is by
/// hand.
#[test]
fn new_branch_never_recreates_a_merged_branch_origin_deleted() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = feat_ahead(&mut ws);
    assert!(matches!(
        only(&ws.push(&["app"])).1,
        PushOutcome::Pushed { .. }
    ));
    let bare = ws.bare("app");
    ws.git(&bare, &["update-ref", "refs/heads/main", &tip]);
    ws.git(&bare, &["update-ref", "-d", "refs/heads/feat"]);
    let remote_before = remote_refs(&ws, "app");
    let config_before = ws.git(&app, &["config", "--get-regexp", "^branch\\."]);

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(only(&run), (Some("feat"), &PushOutcome::NoUpstream));
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "feat").relation, Relation::Gone);
    assert_eq!(branch(e, "feat").unique_commits, 0);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(
        ws.git(&app, &["config", "--get-regexp", "^branch\\."]),
        config_before
    );
    assert_eq!(pushes_served(&ws).len(), 1);
    // the summary says why
    let out = repos(&ws, &app, &["push", "--new-branch"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    // that hint alone, then the footer
    assert_eq!(
        lines[..2],
        [
            "not pushed    app:feat (nothing unique, upstream gone from origin)",
            "              hint: its commits are all on a remote and origin deleted it: \
             recreating it is by hand, never --new-branch",
        ],
        "{text}"
    );
    assert_eq!(lines.len(), 3, "{text}");
    assert!(!ws.has_ref(&bare, "refs/heads/feat"));
}

#[test]
fn new_branch_leaves_a_branch_with_an_upstream_to_the_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = feat_ahead(&mut ws);
    let from = remote_refs(&ws, "app")["refs/heads/feat"].clone();

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(only(&run), (Some("feat"), &pushed(&from, &tip)));
    assert_tracks_origin(&ws, &app, "feat", &tip);
    // and in sync, nothing to create
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(only(&run), (Some("feat"), &PushOutcome::InSync));
    assert_eq!(pushes_served(&ws).len(), 1);
}

#[test]
fn new_branch_creates_only_a_same_named_branch_on_origin() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "old");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.write_registry();
    // tracking origin's `old` under another name, deleted there
    ws.git(
        &app,
        &["switch", "-q", "-c", "renamed", "--track", "origin/old"],
    );
    ws.commit(&app, "renamed");
    ws.upstream_delete_branch("app", "old");
    // and tracking another remote
    ws.git(&app, &["remote", "add", "upstream", &owned_origin("app")]);
    ws.git(&app, &["branch", "-q", "fork"]);
    ws.git(&app, &["config", "branch.fork.remote", "upstream"]);
    ws.git(&app, &["config", "branch.fork.merge", "refs/heads/fork"]);
    let remote_before = remote_refs(&ws, "app");
    let config_before = ws.git(&app, &["config", "--get-regexp", "^branch\\."]);

    for name in ["renamed", "fork"] {
        ws.git(&app, &["switch", "-q", name]);
        let run = ws.push_new_branch(&["app"]);
        assert_eq!(only(&run), (Some(name), &PushOutcome::NoUpstream), "{name}");
    }
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(
        ws.git(&app, &["config", "--get-regexp", "^branch\\."]),
        config_before
    );
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn new_branch_never_overwrites_or_adopts_a_branch_origin_has() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    // origin has a `topic` of its own, which the fetch finds
    let theirs = ws.upstream_commit("app", "topic");
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::RemoteBranchExists { at: theirs }
        )
    );
    assert!(!run.pushes[0].outcome.in_sync());
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_upstream(&app, "topic", "");
    assert_eq!(ws.git(&app, &["rev-parse", "topic"]), tip);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// A run stopped between creating the branch and setting its upstream:
/// the next finds origin's branch at the very commit, reads it up to date,
/// and sets the upstream — whether or not the remote-tracking ref, or half
/// the config, was written.
#[test]
fn new_branch_finishes_a_creation_stopped_before_its_upstream() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    let real_path = ws
        .env()
        .into_iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .unwrap()
        .1;
    let real_path = real_path.to_str().unwrap().to_owned();
    // a `git` first on PATH that fails setting the merge ref, once the
    // remote branch is created and `branch.topic.remote` set
    let bin = ws.outside("wrap-bin");
    write_executable(
        &bin,
        "git",
        &format!(
            "#!/bin/sh
case \" $* \" in
*' config --replace-all branch.topic.merge '*)
	echo 'error: could not lock config file' >&2; exit 255 ;;
esac
PATH='{real_path}' exec git \"$@\"
"
        ),
    );
    ws.set_env("PATH", format!("{}:{real_path}", bin.display()));

    let run = ws.push_new_branch(&["app"]);

    let (_, outcome) = only(&run);
    let PushOutcome::Failed { message } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        message.contains(&format!("refs/heads/topic is on origin at {tip}"))
            && message.contains("could not lock config file")
            && message.contains("rerun repos push --new-branch"),
        "{message}"
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "topic"]), tip);
    assert_eq!(
        ws.git(&app, &["rev-parse", "refs/remotes/origin/topic"]),
        tip
    );
    assert_eq!(ws.git(&app, &["config", "branch.topic.remote"]), "origin");
    ws.assert_upstream(&app, "topic", "");
    // no upstream, so the push says so
    ws.set_env("PATH", real_path);
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("topic"), &PushOutcome::NoUpstream));

    // the rerun: up to date at origin, the upstream set
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (Some("topic"), &PushOutcome::Created { to: tip.clone() })
    );
    assert_tracks_origin(&ws, &app, "topic", &tip);
    assert_eq!(pushes_served(&ws).len(), 2);

    // stopped before the remote-tracking ref too: the fetch writes it, and
    // the same
    ws.git(&app, &["switch", "-q", "-c", "next"]);
    let next = ws.commit(&app, "next");
    let bare = ws.bare("app");
    let from = app.to_str().unwrap();
    ws.git(&bare, &["fetch", "-q", from, "next:refs/heads/next"]);
    assert_eq!(ws.git(&bare, &["rev-parse", "next"]), next);
    assert!(!ws.has_ref(&app, "refs/remotes/origin/next"));
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (Some("next"), &PushOutcome::Created { to: next.clone() })
    );
    assert_tracks_origin(&ws, &app, "next", &next);
}

/// A branch the fetch refspec leaves out would track nothing: never
/// created, a person's to map.
#[test]
fn new_branch_never_creates_a_branch_outside_the_refspec() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = topic(&mut ws);
    ws.git(
        &app,
        &[
            "config",
            "--replace-all",
            "remote.origin.fetch",
            "+refs/heads/main:refs/remotes/origin/main",
        ],
    );
    let remote_before = remote_refs(&ws, "app");

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Unmapped
            }
        )
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_upstream(&app, "topic", "");
    assert!(!ws.has_ref(&app, "refs/remotes/origin/topic"));
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// Never created: in an archived repo (a person's, as a push there is),
/// or under a name git can't take as config.
#[test]
fn new_branch_leaves_an_archived_repo_or_an_unconfigurable_name_alone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "archived = true");
    let app = ws.clone_owned("app", "app", &[]);
    ws.write_registry();
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    ws.commit(&app, "topic");

    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead
            }
        )
    );

    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.write_registry();
    ws.git(&app, &["switch", "-q", "-c", "a=b"]);
    ws.commit(&app, "topic");
    let run = ws.push_new_branch(&["app"]);
    let (_, outcome) = only(&run);
    assert!(
        matches!(outcome, PushOutcome::Failed { message } if message.contains("holds `=`")),
        "{outcome:?}"
    );
    ws.assert_upstream(&app, "a=b", "");
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// What holds a push holds a creation: another live session in the
/// checkout, origin drift, and a detached HEAD has nothing to create.
#[test]
fn new_branch_is_held_as_a_push_is() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = topic(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        app.to_str().unwrap().to_owned(),
        SessionSource::SessionFile,
    )]);
    // arriving after the fetch, re-read right before the creation
    let calls = AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            live.clone()
        }
    };
    let run = ws.push_full(&["app"], &ws.root(), &read, true);
    assert_eq!(
        only(&run),
        (Some("topic"), &PushOutcome::Held { by: SyncHold::Busy })
    );

    ws.git(
        &app,
        &["config", "remote.origin.pushurl", "git@github.com:me/other"],
    );
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::Held {
                by: SyncHold::PushUrl
            }
        )
    );
    ws.git(&app, &["config", "--unset", "remote.origin.pushurl"]);
    ws.set_origin(&app, "other", "git@github.com:me/other");
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::Held {
                by: SyncHold::Entry
            }
        )
    );
    ws.set_origin(&app, "app", &owned_origin("app"));
    ws.git(&app, &["switch", "-q", "--detach", "HEAD"]);
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(only(&run), (None, &PushOutcome::Detached));

    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_upstream(&app, "topic", "");
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

// --- the races ---

#[test]
fn a_remote_rewound_after_the_fetch_is_never_overwritten() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let base = ws.git(&app, &["rev-parse", "main"]);
    let fetched = ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.git(
        &app,
        &["merge", "-q", "--ff-only", "refs/remotes/origin/main"],
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), fetched);
    let tip = ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    let env = ws.env();
    let bare = ws.bare("app");
    // after the fetch, another hand rewinds the remote to an ancestor of
    // the commit: a plain push would fast-forward it
    let read = reader_then(2, || {
        git_env(&env, &bare, &["update-ref", "refs/heads/main", &base]);
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::Held {
                by: SyncHold::Changed
            }
        )
    );
    // the lease refused it, at the remote: reached, and left rewound
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), base);
    assert_eq!(pushes_served(&ws).len(), 1);
    assert_eq!(
        ws.git(&app, &["rev-parse", "refs/remotes/origin/main"]),
        fetched
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip);
}

#[test]
fn a_remote_branch_deleted_after_the_fetch_is_never_recreated() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = feat_ahead(&mut ws);
    let env = ws.env();
    let bare = ws.bare("app");
    let read = reader_then(2, || {
        git_env(&env, &bare, &["update-ref", "-d", "refs/heads/feat"]);
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    assert_eq!(
        only(&run),
        (
            Some("feat"),
            &PushOutcome::Held {
                by: SyncHold::Changed
            }
        )
    );
    assert!(!remote_refs(&ws, "app").contains_key("refs/heads/feat"));
    assert_eq!(pushes_served(&ws).len(), 1);
    // the rerun reads it gone: nothing to push to
    let run = ws.push(&["app"]);
    assert_eq!(only(&run), (Some("feat"), &PushOutcome::NoUpstream));
    assert!(!remote_refs(&ws, "app").contains_key("refs/heads/feat"));
    assert_eq!(ws.git(&app, &["rev-parse", "feat"]), tip);
    assert_eq!(pushes_served(&ws).len(), 1);
}

#[test]
fn a_branch_created_on_origin_after_the_fetch_is_never_overwritten() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    let env = ws.env();
    let bare = ws.bare("app");
    let main = ws.git(&bare, &["rev-parse", "main"]);
    let read = reader_then(2, || {
        git_env(&env, &bare, &["update-ref", "refs/heads/topic", &main]);
    });

    let run = ws.push_full(&["app"], &ws.root(), &read, true);

    // the lease on no such ref refused it, at the remote
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::Held {
                by: SyncHold::Changed
            }
        )
    );
    assert_eq!(ws.git(&bare, &["rev-parse", "topic"]), main);
    assert_eq!(pushes_served(&ws).len(), 1);
    ws.assert_upstream(&app, "topic", "");
    assert!(!ws.has_ref(&app, "refs/remotes/origin/topic"));
    // the rerun's fetch finds it: never adopted
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(
        only(&run),
        (
            Some("topic"),
            &PushOutcome::RemoteBranchExists { at: main.clone() }
        )
    );
    assert_eq!(ws.git(&bare, &["rev-parse", "topic"]), main);
    assert_eq!(ws.git(&app, &["rev-parse", "topic"]), tip);
    assert_eq!(pushes_served(&ws).len(), 1);
}

/// The branch re-read right before the creation: a commit made on it, or
/// an upstream configured for it, since classifying holds it.
#[test]
fn a_branch_changed_after_the_fetch_is_never_created() {
    // (what changes, in the checkout)
    type Change = fn(&[(OsString, OsString)], &Path);
    let cases: [(&str, Change); 2] = [
        ("a commit", |env, app| {
            git_env(env, app, &["commit", "-q", "--allow-empty", "-m", "late"]);
        }),
        // another remote's, which resolves no remote-tracking ref: only the
        // config says it
        ("an upstream", |env, app| {
            git_env(env, app, &["config", "branch.topic.remote", "elsewhere"]);
            git_env(
                env,
                app,
                &["config", "branch.topic.merge", "refs/heads/topic"],
            );
        }),
    ];
    for (case, change) in cases {
        let mut ws = FixtureWorkspace::new();
        let (app, _) = topic(&mut ws);
        let env = ws.env();
        let read = reader_then(2, || change(&env, &app));

        let run = ws.push_full(&["app"], &ws.root(), &read, true);

        assert_eq!(
            only(&run),
            (
                Some("topic"),
                &PushOutcome::Held {
                    by: SyncHold::Changed
                }
            ),
            "{case}"
        );
        assert!(!ws.has_ref(&ws.bare("app"), "refs/heads/topic"), "{case}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_commit_another_hand_pushed_meanwhile_reads_in_sync() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let env = ws.env();
    let bare = ws.bare("app");
    let url = format!("file://{}", bare.display());
    let refspec = format!("{tip}:refs/heads/main");
    // the very commit reaches origin after the re-checks' fetch, unfetched
    let read = reader_then(2, || {
        git_env(&env, &app, &["push", "-q", &url, &refspec]);
    });

    let run = ws.push_with(&["app"], &ws.root(), &read);

    // git's `up to date`: nothing sent, the branch where it was asked
    assert_eq!(only(&run), (Some("main"), &PushOutcome::InSync));
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), tip);
    assert_eq!(pushes_served(&ws).len(), 1);
    // the remote-tracking ref moved to it as a push of its own would: in
    // sync from local refs, no fetch
    assert_eq!(
        ws.git(&app, &["rev-parse", "refs/remotes/origin/main"]),
        tip
    );
    ws.assert_track(&app, "main", "");
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
}

#[test]
fn a_rewrite_made_after_the_push_url_is_read_never_redirects_the_push() {
    let mut ws = FixtureWorkspace::new();
    // where a redirected push would land
    ws.remote("other", &[]);
    let (app, tip) = ahead(&mut ws);
    let real_path = ws
        .env()
        .into_iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .unwrap()
        .1;
    let real_path = real_path.to_str().unwrap().to_owned();
    // a `git` first on PATH that, once the push URL has been read right
    // before the push (the probe reads it first), writes every config that
    // would move a `git push` to the registry's URL elsewhere
    let bin = ws.outside("wrap-bin");
    let count = ws.outside("get-url.count");
    let app_s = app.to_str().unwrap();
    let url = "git@github.com:me/app";
    let other = "git@github.com:me/other";
    write_executable(
        &bin,
        "git",
        &format!(
            "#!/bin/sh
PATH='{real_path}' git \"$@\"
status=$?
case \" $* \" in
*' remote get-url --push --all origin '*)
	echo x >> '{count}'
	if [ \"$(wc -l < '{count}')\" -eq 2 ]; then
		g() {{ PATH='{real_path}' git -C '{app_s}' config \"$@\"; }}
		g --unset-all 'url.{url}.pushInsteadOf'
		g 'url.{other}.pushInsteadOf' '{url}'
		g 'url.{other}.insteadOf' '{url}'
		g 'remote.{url}.url' '{other}'
		g 'remote.{url}.pushurl' '{other}'
	fi ;;
esac
exit $status
",
            count = count.display()
        ),
    );
    ws.set_env("PATH", format!("{}:{real_path}", bin.display()));
    let app_before = remote_refs(&ws, "app");
    let other_before = remote_refs(&ws, "other");

    let run = ws.push(&["app"]);

    // read twice, the rewrites written after the second
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "x\nx\n");
    assert_eq!(
        ws.git(&app, &["remote", "get-url", "--push", "--all", "origin"]),
        other
    );
    assert_eq!(ws.git(&app, &["ls-remote", "--get-url", url]), other);
    // the push went to the registry's repo all the same
    assert_eq!(
        only(&run),
        (Some("main"), &pushed(&app_before["refs/heads/main"], &tip))
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&app_before, &[("refs/heads/main", &tip)])
    );
    assert_eq!(remote_refs(&ws, "other"), other_before);
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/app'"]);
}

// --- the binary ---

fn repos(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> Output {
    ws.command(REPOS, cwd).args(args).output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// `repos push --json`'s fatal-error document, after checking the exit.
fn error_doc(out: &Output, code: i32) -> Value {
    assert_eq!(out.status.code(), Some(code), "stderr: {}", stderr(out));
    let doc: Value = serde_json::from_str(&stdout(out)).unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    doc
}

#[test]
fn push_from_the_cwd_exits_zero_once_in_sync() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    std::fs::create_dir(app.join("sub")).unwrap();

    let out = repos(&ws, &app.join("sub"), &["push"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "pushed        app +1", "{text}");
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), tip);

    let out = repos(&ws, &app, &["push"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("in sync       app\n"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn push_exits_one_when_a_branch_isnt_pushed() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    let blog = ws.owned_repo("blog", &[]);
    ws.git(&blog, &["switch", "-q", "-c", "topic"]);
    ws.commit(&blog, "topic");
    ws.assert_upstream(&blog, "topic", "");
    ws.write_registry();
    let app_was = ws.git(&app, &["rev-parse", "main"]);

    let out = repos(&ws, &ws.root(), &["push", "app", "blog"]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(
            "not pushed    app (behind 1)  blog:topic (no upstream on origin)\n              \
             hint: repos sync fast-forwards a branch behind its upstream (and moves a stale \
             shallow one)\n              \
             hint: the user creates it on origin with repos push --new-branch (an agent \
             can't)\n"
        ),
        "{text}"
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), app_was);
    ws.assert_head(&blog, Some("topic"));
    assert!(!ws.has_ref(&ws.bare("blog"), "refs/heads/topic"));

    let doc: Value = serde_json::from_str(&stdout(&repos(
        &ws,
        &ws.root(),
        &["push", "--json", "app", "blog"],
    )))
    .unwrap();
    let kinds: Vec<&str> = doc["pushes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["not_ahead", "no_upstream"]);
}

#[test]
fn a_branch_in_sync_on_refs_that_may_not_be_origins_is_held() {
    // (setup on a workspace with `app` in sync, the hold it reads, its text)
    type Unverified = fn(&FixtureWorkspace, &Path);
    let cases: [(&str, Unverified, &str, &str); 2] = [
        (
            "fetch failed",
            |ws, _| std::fs::remove_dir_all(ws.bare("app")).unwrap(),
            "fetch_failed",
            "held          app (fetch failed)",
        ),
        (
            "origin drift",
            |ws, app| {
                // another repo with the same history, the branch in sync
                // with it
                let from = format!("file://{}", ws.bare("app").display());
                let other = ws.bare("other");
                ws.git(
                    ws.base(),
                    &["clone", "-q", "--bare", &from, other.to_str().unwrap()],
                );
                ws.set_origin(app, "other", "git@github.com:me/other");
                ws.git(app, &["fetch", "-q", "origin"]);
                ws.assert_track(app, "main", "");
            },
            "entry",
            "held          app",
        ),
    ];
    for (case, unverify, by, held) in cases {
        let mut ws = FixtureWorkspace::new();
        let app = ws.owned_repo("app", &[]);
        ws.write_registry();
        ws.assert_track(&app, "main", "");
        unverify(&ws, &app);

        let out = repos(&ws, &ws.root(), &["push", "--json", "app"]);

        assert_eq!(out.status.code(), Some(1), "{case}: {}", stderr(&out));
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        let p = &doc["pushes"][0];
        assert_eq!(
            (&p["kind"], &p["by"]),
            (&"held".into(), &by.into()),
            "{case}"
        );
        let out = repos(&ws, &ws.root(), &["push", "app"]);
        assert_eq!(out.status.code(), Some(1), "{case}");
        let text = stdout(&out);
        assert!(text.lines().any(|l| l == held), "{case}: {text}");
        assert!(!text.contains("in sync"), "{case}: {text}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_held_push_keeps_syncs_label_whatever_else_holds_it() {
    // ahead, origin pushing elsewhere, and the fetch failing: sync names
    // the push URL first, and so does the push
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    ws.git(
        &app,
        &["config", "remote.origin.pushurl", "git@github.com:me/other"],
    );
    std::fs::remove_dir_all(ws.bare("app")).unwrap();

    let run = ws.push(&["app"]);

    let e = find_entry(&run.entries, "app");
    assert!(e.fetch_error.is_some());
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::PushUrl,
        }
    );
    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::Held {
                by: SyncHold::PushUrl
            }
        )
    );
    assert!(matches!(run.pushes[0].fetch, FetchOutcome::Failed { .. }));
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn push_json_is_the_versioned_outcome_report() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let from = remote_refs(&ws, "app")["refs/heads/main"].clone();

    let out = repos(&ws, &ws.root(), &["push", "--json", "app"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    assert_eq!(doc["status"]["version"], fuz_repos::STATUS_FORMAT_VERSION);
    assert_eq!(doc["status"]["fetched"], true);
    assert_eq!(doc["status"]["unregistered"], Value::Null);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
            "kind": "pushed",
            "from": from,
            "to": tip,
        }])
    );
}

#[test]
fn push_usage_errors_exit_two() {
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    // an owned reference, pinned
    ws.remote("wpt", &[]);
    ws.declare_reference("wpt", support::OWNER, "wpt", "pinned = true");
    ws.clone_owned("wpt", "wpt", &[]);
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party("lib", "lib", &[]);
    ws.write_registry();
    let ssh_before = ws.ssh_log();

    // the cwd in no entry's checkout
    let out = repos(&ws, &ws.root(), &["push"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out).lines().next().unwrap(),
        format!(
            "error: {} is in no registry entry's checkout",
            ws.root().display()
        )
    );
    let doc = error_doc(&repos(&ws, &ws.root(), &["push", "--json"]), 2);
    assert_eq!(doc["error"]["kind"], "no_checkout");
    // an unknown target
    let doc = error_doc(&repos(&ws, &ws.root(), &["push", "--json", "ap"]), 2);
    assert_eq!(doc["error"]["kind"], "unknown_entry");
    // a third-party reference, by key or from inside it, and a pin
    for (cwd, target) in [(ws.root(), "lib"), (lib, ".")] {
        let doc = error_doc(&repos(&ws, &cwd, &["push", "--json", target]), 2);
        assert_eq!(
            doc["error"],
            serde_json::json!({
                "kind": "push_third_party",
                "key": "lib",
                "message": "`lib` is a third-party reference, which repos never pushes",
                "hint": "repos push takes the registry's owned repos; a reference's commits \
                         stay local",
            })
        );
    }
    let doc = error_doc(
        &repos(&ws, &ws.root(), &["push", "--json", "app", "wpt"]),
        2,
    );
    assert_eq!(doc["error"]["kind"], "push_pinned");
    assert_eq!(doc["error"]["key"], "wpt");
    // refused before anything reached a remote
    assert_eq!(ws.ssh_log(), ssh_before);
}

#[test]
fn an_agent_pushes_through_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let blog = ws.owned_repo("blog", &[]);
    ws.commit(&blog, "local");
    ws.write_registry();
    let blog_was = ws.git(&ws.bare("blog"), &["rev-parse", "main"]);

    let out = ws
        .command(REPOS, &app)
        .env("CLAUDECODE", "1")
        .args(["push"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("pushed        app +1\n"),
        "{}",
        stdout(&out)
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), tip);
    // the other repo, only sync's to push
    assert_eq!(ws.git(&ws.bare("blog"), &["rev-parse", "main"]), blog_was);
}

#[test]
fn a_pushed_status_reads_in_sync_without_a_fetch() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "main").relation, Relation::Ahead { commits: 1 });

    let run = ws.push(&["app"]);
    assert!(matches!(only(&run).1, PushOutcome::Pushed { .. }));

    // local refs only: the remote-tracking ref the push moved
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
    assert_eq!(branch(&e, "main").verdict, Verdict::Quiet);
    ws.assert_track(&app, "main", "");
}

#[test]
fn new_branch_is_the_users_and_refused_to_an_agent() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    let ssh_before = ws.ssh_log();
    let agent = |args: &[&str]| {
        ws.command(REPOS, &app)
            .env("CLAUDECODE", "1")
            .args(args)
            .output()
            .unwrap()
    };

    let out = agent(&["push", "--new-branch"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: creating a remote branch is the user's: repos push --new-branch doesn't run \
         in an agent's shell (CLAUDECODE is set)\n\
         hint: the user runs repos push --new-branch themselves; an agent pushes a branch \
         origin already has with repos push\n"
    );
    let doc = error_doc(&agent(&["push", "--new-branch", "--json"]), 2);
    assert_eq!(doc["error"]["kind"], "new_branch_by_agent");
    // refused before anything ran: not even the fetch
    assert_eq!(ws.ssh_log(), ssh_before);
    // the agent's own push says whose it is
    let out = agent(&["push"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "not pushed    app:topic (no upstream on origin)\n              hint: the user \
             creates it on origin with repos push --new-branch (an agent can't)\n"
        ),
        "{}",
        stdout(&out)
    );
    assert!(!ws.has_ref(&ws.bare("app"), "refs/heads/topic"));

    // the user's
    let out = repos(&ws, &app, &["push", "--new-branch"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(
        text.lines().next(),
        Some("pushed        app:topic (new branch)"),
        "{text}"
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "topic"]), tip);
    assert_tracks_origin(&ws, &app, "topic", &tip);
    let doc: Value = serde_json::from_str(&stdout(&repos(
        &ws,
        &app,
        &["push", "--json", "--new-branch"],
    )))
    .unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    assert_eq!(doc["pushes"][0]["kind"], "in_sync");
}
