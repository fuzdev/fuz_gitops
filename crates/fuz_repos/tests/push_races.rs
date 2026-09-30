//! The races the lease and the direct URL close: origin or the checkout
//! changing between the fetch and the push. Each run is followed by the exact
//! refs it should leave on both sides.
//!
//! Pushes reach the local bare remotes over the fixture's own `ssh`, which
//! serves the registry's SSH URLs (the support module says how). The
//! live-sessions reader is the seam for what happens between classifying and
//! pushing: its second call is the one right before the push.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::report::{PushOutcome, SyncHold};
use fuz_repos::sessions::LiveSessions;
use fuz_repos::state::Relation;
use support::push::{
    ahead, feat_ahead, only, pushed, pushes_served, quiet, remote_refs, topic, with,
};
use support::{FixtureWorkspace, branch, find_entry, write_executable};

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
