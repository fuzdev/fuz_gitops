//! `repos push --new-branch` through the library: creating a branch on origin
//! and tracking it, and when that is refused. Each run is followed by the exact
//! refs it should leave on both sides.
//!
//! Pushes reach the local bare remotes over the fixture's own `ssh`, which
//! serves the registry's SSH URLs (the support module says how).

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::{BranchSyncHold, PushOutcome};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchNeedsHuman, Relation};
use support::cli::{repos, stderr, stdout};
use support::push::{
    assert_tracks_origin, feat_ahead, only, pushed, pushes_served, quiet, remote_refs, topic, with,
};
use support::{FixtureWorkspace, LiveChild, branch, find_entry, owned_origin, write_executable};

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

/// The branch the entry follows, renamed away on origin (`master` to
/// `main`) with a local commit on it: a person repoints it, and neither a
/// push nor `--new-branch` puts it back.
#[test]
fn new_branch_never_recreates_the_entrys_own_branch_gone_from_origin() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "branch = \"master\"");
    let up = ws.upstream("app");
    ws.git(&up, &["push", "-q", "origin", "main:master"]);
    let app = ws.clone_owned("app", "app", &["--branch", "master"]);
    ws.upstream_delete_branch("app", "master");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.commit(&app, "local");
    ws.assert_track(&app, "master", "[gone]");
    ws.assert_count(&app, &["master", "--not", "--remotes"], 1);
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");
    assert!(!remote_before.contains_key("refs/heads/master"));
    let config_before = ws.git(&app, &["config", "--get-regexp", "^branch\\."]);

    for run in [ws.push(&["app"]), ws.push_new_branch(&["app"])] {
        assert_eq!(only(&run), (Some("master"), &PushOutcome::NoUpstream));
        let e = find_entry(&run.entries, "app");
        assert_eq!(
            e.needs_human,
            [NeedsHuman::DefaultBranchGone {
                branch: "master".into()
            }]
        );
    }
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(
        ws.git(&app, &["config", "--get-regexp", "^branch\\."]),
        config_before
    );
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    // the summary says why, never to create it
    for args in [&["push"][..], &["push", "--new-branch"]] {
        let out = repos(&ws, &app, args);
        assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[..lines.len() - 1],
            [
                "needs human   app (master's upstream is gone from origin)",
                "not pushed    app (the entry's branch, upstream gone from origin)",
                "              hint: the entry's own branch is gone from origin (its default \
                 renamed?): repoint the registry's branch and the checkout by hand; \
                 --new-branch never recreates it",
            ],
            "{text}"
        );
    }
    assert_eq!(remote_refs(&ws, "app"), remote_before);
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

/// Never created in an archived repo: a person's, as a push there is.
#[test]
fn new_branch_leaves_an_archived_repo_alone() {
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
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

/// A name holding `=` is created and tracked as any other: the upstream
/// it would track is read with the config whole (`--config-env`), where
/// `-c` would split it at its first `=`.
#[test]
fn new_branch_creates_a_name_holding_an_equals_sign() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.write_registry();
    ws.git(&app, &["switch", "-q", "-c", "a=b"]);
    let tip = ws.commit(&app, "topic");
    ws.assert_upstream(&app, "a=b", "");

    let run = ws.push_new_branch(&["app"]);

    assert_eq!(
        only(&run),
        (Some("a=b"), &PushOutcome::Created { to: tip.clone() })
    );
    assert_eq!(
        ws.git(&ws.bare("app"), &["rev-parse", "refs/heads/a=b"]),
        tip
    );
    assert_tracks_origin(&ws, &app, "a=b", &tip);
    assert_eq!(
        ws.git(&app, &["config", "--get", "branch.a=b.merge"]),
        "refs/heads/a=b"
    );
    // a rerun has nothing to do
    let run = ws.push_new_branch(&["app"]);
    assert_eq!(only(&run), (Some("a=b"), &PushOutcome::InSync));
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
        (
            Some("topic"),
            &PushOutcome::Held {
                by: BranchSyncHold::Busy
            }
        )
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
                by: BranchSyncHold::PushUrl
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
                by: BranchSyncHold::Entry
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
