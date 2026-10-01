//! `repos sync`'s pushes over fixture workspaces: a branch ahead pushed to
//! its upstream on the registry's repo — the exact commit classified,
//! nothing else — and every way a push is held, refused, or fails, each run
//! followed by the exact refs it should leave on both sides. (`repos push`
//! makes the same push: the `push_*` tests have their own.)
//!
//! Pushes reach the local bare remotes over the fixture's own `ssh`, which
//! serves the registry's SSH URLs (the support module says how), so the
//! push runs as it would for real: to the registry's URL, SSH only.
//! The live-sessions reader is the seam for what happens between
//! classifying and pushing, as in `sync.rs`.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::classify::NeedsHuman;
use fuz_repos::remote::{RemoteFailure, UnreachableCause};
use fuz_repos::report::{BranchOutcome, BranchSyncHold};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchHold, BranchNeedsHuman, SyncAction, Verdict};
use fuz_repos::sync::SyncRun;
use support::{FixtureWorkspace, LiveChild, branch, find_entry, write_executable};

const fn push(commits: u32) -> SyncAction {
    SyncAction::Push { commits }
}

const fn quiet() -> LiveSessions {
    LiveSessions::Known(Vec::new())
}

fn outcome<'a>(run: &'a SyncRun, key: &str, name: &str) -> &'a BranchOutcome {
    &run.outcomes
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no outcomes for {key}: {:?}", run.outcomes))
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch {key}:{name}: {:?}", run.outcomes))
        .outcome
}

const fn held(action: SyncAction, by: BranchSyncHold) -> BranchOutcome {
    BranchOutcome::Held { action, by }
}

fn pushed(from: &str, to: &str) -> BranchOutcome {
    BranchOutcome::Pushed {
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
/// is the one after the fetches, the second right before the first action)
/// first runs `then`.
fn reader_then(at: usize, then: impl Fn() + Sync) -> impl Fn() -> LiveSessions + Sync {
    let calls = AtomicUsize::new(0);
    move || {
        if calls.fetch_add(1, Ordering::SeqCst) + 1 == at {
            then();
        }
        quiet()
    }
}

/// Moves `branch` one commit forward — a child with the same tree —
/// without touching HEAD or the files. Returns the new commit.
fn advance(ws: &FixtureWorkspace, repo: &Path, branch: &str) -> String {
    let tree = format!("refs/heads/{branch}^{{tree}}");
    let parent = format!("refs/heads/{branch}");
    let oid = ws.git(repo, &["commit-tree", &tree, "-p", &parent, "-m", "local"]);
    ws.git(repo, &["update-ref", &parent, &oid]);
    oid
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

#[test]
fn pushes_the_branch_ahead_and_nothing_else() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // a tracked hooks dir the repo's config names, one hook per step a push
    // takes locally
    let hooks_log = ws.base().join("hooks.log");
    for hook in ["pre-push", "reference-transaction", "post-checkout"] {
        write_executable(
            &app,
            &format!(".githooks/{hook}"),
            &format!("#!/bin/sh\necho {hook} >> '{}'\n", hooks_log.display()),
        );
    }
    let tip = ws.commit(&app, "local");
    // tags a push could carry along: an annotated one on the tip, which
    // `push.followTags` sends, and a lightweight one
    ws.git(&app, &["tag", "-a", "v9", "-m", "v9"]);
    ws.git(&app, &["tag", "light"]);
    ws.git(&app, &["config", "push.followTags", "true"]);
    // a branch ahead of another remote's: never pushed
    let url = format!("file://{}", ws.bare("app").display());
    ws.git(&app, &["remote", "add", "upstream", &url]);
    ws.git(&app, &["fetch", "-q", "upstream"]);
    ws.git(
        &app,
        &["branch", "-q", "--track", "theirs", "upstream/main"],
    );
    ws.git(&app, &["update-ref", "refs/heads/theirs", &tip]);
    ws.assert_track(&app, "theirs", "[ahead 1]");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.git(&app, &["config", "core.hooksPath", ".githooks"]);
    ws.write_registry();
    assert!(!hooks_log.exists());
    let remote_before = remote_refs(&ws, "app");
    let local_before = ws.refs(&app);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    assert_eq!(branch(e, "theirs").verdict, Verdict::LocalOnly);
    assert_eq!(
        outcome(&run, "app", "main"),
        &pushed(&remote_before["refs/heads/main"], &tip)
    );
    assert_eq!(outcome(&run, "app", "theirs"), &BranchOutcome::Untouched);
    // the remote's branch moved to exactly the commit, and nothing else on
    // the remote changed: no tag, no other branch
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", &tip)])
    );
    // locally, git's own record of the push: origin's remote-tracking ref
    // (and `origin/HEAD`, the symref to it)
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
    ws.assert_track(&app, "main", "");
    // over SSH to the registry's repo, in batch mode, once
    let log = ws.ssh_push_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].contains("BatchMode=yes"), "{log:?}");
    assert!(
        log[0].ends_with("git@github.com git-receive-pack 'me/app'"),
        "{log:?}"
    );
    // no hook ran
    assert!(!hooks_log.exists());

    // again: in sync, nothing to push
    let run = ws.sync();
    assert_eq!(outcome(&run, "app", "main"), &BranchOutcome::Untouched);
    assert_eq!(ws.ssh_push_log().len(), 1);
    assert!(!hooks_log.exists());
}

#[test]
fn a_commit_landing_after_classifying_is_never_pushed() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    let env = ws.env();
    let landed = std::sync::Mutex::new(String::new());
    let read = reader_then(2, || {
        git_env(
            &env,
            &app,
            &["commit", "-q", "--allow-empty", "-m", "landed"],
        );
        *landed.lock().unwrap() = git_env(&env, &app, &["rev-parse", "HEAD"]);
    });

    let run = ws.sync_with(4, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::Changed)
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(
        ws.git(&app, &["rev-parse", "main"]),
        *landed.lock().unwrap()
    );
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

/// A change to a clone between classifying and pushing: the fixture's
/// environment, the clone, and the commit its `main` holds.
type Change = fn(&[(OsString, OsString)], &Path, &str);

#[test]
fn a_branch_reconfigured_after_classifying_is_held() {
    // each leaves the commit as it was: only the branch's upstream, where
    // it goes on the remote, or what the ref is has changed
    let cases: [(&str, Change); 4] = [
        ("merge", |env, app, _| {
            git_env(
                env,
                app,
                &["config", "branch.main.merge", "refs/heads/other"],
            );
        }),
        ("remote", |env, app, _| {
            git_env(
                env,
                app,
                &["remote", "add", "fork", "git@github.com:me/other"],
            );
            git_env(env, app, &["config", "branch.main.remote", "fork"]);
        }),
        ("fetch refspec", |env, app, _| {
            git_env(
                env,
                app,
                &[
                    "config",
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/remotes/mirror/*",
                ],
            );
        }),
        ("symref", |env, app, tip| {
            git_env(env, app, &["update-ref", "refs/heads/other", tip]);
            git_env(
                env,
                app,
                &["symbolic-ref", "refs/heads/main", "refs/heads/other"],
            );
        }),
    ];
    for (case, change) in cases {
        let mut ws = FixtureWorkspace::new();
        ws.remote("other", &[]);
        let (app, tip) = ahead(&mut ws);
        let app_before = remote_refs(&ws, "app");
        let other_before = remote_refs(&ws, "other");
        let env = ws.env();
        let read = reader_then(2, || change(&env, &app, &tip));

        let run = ws.sync_with(4, &read);

        let e = find_entry(&run.entries, "app");
        assert_eq!(
            branch(e, "main").verdict,
            Verdict::Act { action: push(1) },
            "{case}"
        );
        assert_eq!(
            outcome(&run, "app", "main"),
            &held(push(1), BranchSyncHold::Changed),
            "{case}"
        );
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip, "{case}");
        assert_eq!(remote_refs(&ws, "app"), app_before, "{case}");
        assert_eq!(remote_refs(&ws, "other"), other_before, "{case}");
        assert_eq!(ws.ssh_push_log(), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_remote_moved_since_the_fetch_refuses_the_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let env = ws.env();
    let up = ws.upstream("app");
    let moved = std::sync::Mutex::new(String::new());
    // another hand pushes after sync's fetch
    let read = reader_then(2, || {
        git_env(
            &env,
            &up,
            &["commit", "-q", "--allow-empty", "-m", "theirs"],
        );
        git_env(&env, &up, &["push", "-q", "origin", "main"]);
        *moved.lock().unwrap() = git_env(&env, &up, &["rev-parse", "HEAD"]);
    });

    let run = ws.sync_with(4, &read);

    // git refused the push, which isn't a fast-forward now: rerun
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::Changed)
    );
    let moved = moved.lock().unwrap().clone();
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), moved);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip);
    // the remote was reached, and refused it
    assert_eq!(ws.ssh_push_log().len(), 1);

    // the rerun sees it diverged: a person's
    let run = ws.sync();
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), moved);
}

#[test]
fn a_remote_branch_deleted_after_the_fetch_is_never_recreated() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    let tip = advance(&ws, &app, "feat");
    ws.assert_track(&app, "feat", "[ahead 1]");
    ws.write_registry();
    let env = ws.env();
    let bare = ws.bare("app");
    // another hand deletes it after sync's fetch
    let read = reader_then(2, || {
        git_env(&env, &bare, &["update-ref", "-d", "refs/heads/feat"]);
    });

    let run = ws.sync_with(4, &read);

    // the lease on the fetched tip refused it: rerun
    assert_eq!(
        outcome(&run, "app", "feat"),
        &held(push(1), BranchSyncHold::Changed)
    );
    assert!(!remote_refs(&ws, "app").contains_key("refs/heads/feat"));
    assert_eq!(ws.ssh_push_log().len(), 1);

    // the rerun reads its upstream gone: cleanup, never a push
    let run = ws.sync();
    assert_eq!(outcome(&run, "app", "feat"), &BranchOutcome::Untouched);
    let e = find_entry(&run.entries, "app");
    assert!(matches!(branch(e, "feat").verdict, Verdict::Cleanup { .. }));
    assert!(!remote_refs(&ws, "app").contains_key("refs/heads/feat"));
    assert_eq!(ws.git(&app, &["rev-parse", "feat"]), tip);
    assert_eq!(ws.ssh_push_log().len(), 1);
}

/// A fixture change to a clone's config: the workspace, the clone, and a
/// config key a case may use.
type Setup = fn(&FixtureWorkspace, &Path, &str);

#[test]
fn a_push_url_other_than_the_registrys_is_refused() {
    let app_ssh = "git@github.com:me/app";
    let other_ssh = "git@github.com:me/other";
    let redirect = format!("url.{other_ssh}.pushInsteadOf");
    // each a config that sends the push elsewhere, and where it goes
    let cases: [(&str, Setup, Vec<&str>); 4] = [
        (
            "pushurl",
            |ws, app, _| {
                ws.git(
                    app,
                    &["config", "remote.origin.pushurl", "git@github.com:me/other"],
                );
            },
            vec![other_ssh],
        ),
        (
            "pushInsteadOf",
            |ws, app, redirect| {
                ws.git(app, &["config", redirect, "git@github.com:me/app"]);
            },
            vec![other_ssh],
        ),
        // the registry's repo among them, and another
        (
            "several",
            |ws, app, _| {
                ws.git(
                    app,
                    &[
                        "config",
                        "remote.origin.pushurl",
                        "ssh://git@github.com/me/app",
                    ],
                );
                ws.git(
                    app,
                    &[
                        "config",
                        "--add",
                        "remote.origin.pushurl",
                        "git@github.com:me/other",
                    ],
                );
            },
            vec!["ssh://git@github.com/me/app", other_ssh],
        ),
        (
            "https",
            |ws, app, _| {
                ws.git(
                    app,
                    &[
                        "config",
                        "remote.origin.pushurl",
                        "https://github.com/me/app",
                    ],
                );
            },
            vec!["https://github.com/me/app"],
        ),
    ];
    for (case, setup, urls) in cases {
        let mut ws = FixtureWorkspace::new();
        // where a misdirected push would land
        ws.remote("other", &[]);
        let (app, _) = ahead(&mut ws);
        setup(&ws, &app, &redirect);
        assert_eq!(
            ws.git(&app, &["remote", "get-url", "--push", "--all", "origin"]),
            urls.join("\n"),
            "{case}"
        );
        let app_before = remote_refs(&ws, "app");
        let other_before = remote_refs(&ws, "other");

        // the preview says so
        let e = find_entry(&ws.status(), "app").clone();
        assert_eq!(
            e.needs_human,
            [NeedsHuman::PushUrlMismatch {
                push_urls: urls.iter().map(|u| (*u).to_owned()).collect(),
                expected: app_ssh.into(),
            }],
            "{case}"
        );
        assert_eq!(
            branch(&e, "main").verdict,
            Verdict::Held {
                action: push(1),
                by: BranchHold::PushUrl
            },
            "{case}"
        );

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &held(push(1), BranchSyncHold::PushUrl),
            "{case}"
        );
        assert_eq!(remote_refs(&ws, "app"), app_before, "{case}");
        assert_eq!(remote_refs(&ws, "other"), other_before, "{case}");
        assert_eq!(ws.ssh_push_log(), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_push_url_that_only_spells_the_registrys_is_refused() {
    // each names `github.com/me/app` in its text; git connects elsewhere,
    // or names it in a way the registry's URLs never do
    let lookalikes = [
        "ssh://evil.com/x@github.com/me/app",
        "evil.com:x@github.com/me/app",
        "ssh+git://evil.com/@github.com/me/app",
        "git@evil.com:git@github.com:me/app",
        "ssh://evil.com%2F@github.com/me/app",
        "ssh://git%40evil.com@github.com/me/app",
        "git@github.com:me%2Fapp",
        "ssh://a@b@github.com/me/app",
        "git@[::1]:me/app",
        "[git@github.com]:me/app",
        "ssh://git@github.com:22/me/app",
        "-oProxyCommand=x@github.com:me/app",
        // brackets, where git connects to `evil.com` (or `x`)
        "ssh://[evil.com]x@github.com/me/app",
        "ssh://[evil.com]x@github.com:2222/me/app",
        "[evil.com]x@github.com:me/app",
        // (past the fixture's fetch rewrite, which `git@` would hit)
        "x@github.com:me/app@[x]:y",
    ];
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    for url in lookalikes {
        ws.git(&app, &["config", "remote.origin.pushurl", url]);
        assert_eq!(
            ws.git(&app, &["remote", "get-url", "--push", "--all", "origin"]),
            url
        );
        let e = find_entry(&ws.status(), "app").clone();
        assert_eq!(
            e.needs_human,
            [NeedsHuman::PushUrlMismatch {
                push_urls: vec![url.to_owned()],
                expected: "git@github.com:me/app".into(),
            }],
            "{url}"
        );

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &held(push(1), BranchSyncHold::PushUrl),
            "{url}"
        );
        assert_eq!(remote_refs(&ws, "app"), remote_before, "{url}");
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip, "{url}");
        assert_eq!(ws.ssh_push_log(), Vec::<String>::new(), "{url}");
    }
}

#[test]
fn a_remote_tracking_ref_moved_after_classifying_is_held() {
    // two commits ahead; after classifying, the first reaches origin and
    // another fetch records it: still a fast-forward, but no longer what
    // the verdict counted
    let mut ws = FixtureWorkspace::new();
    let (app, first) = ahead(&mut ws);
    let second = ws.commit(&app, "second");
    ws.assert_track(&app, "main", "[ahead 2]");
    let env = ws.env();
    let bare = ws.bare("app");
    let url = format!("file://{}", bare.display());
    let refspec = format!("{first}:refs/heads/main");
    let read = reader_then(2, || {
        git_env(&env, &app, &["push", "-q", &url, &refspec]);
        git_env(&env, &app, &["fetch", "-q", "origin"]);
    });

    let run = ws.sync_with(4, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(2) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(2), BranchSyncHold::Changed)
    );
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), first);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), second);
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn a_fetch_after_classifying_never_turns_the_push_into_a_force() {
    // after classifying, another hand pushes a sibling of the local commit
    // and a fetch records it: the count ahead is the same, and the lease
    // would pass — only the fetched tip no longer being an ancestor of the
    // commit keeps the push from overwriting it
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let env = ws.env();
    let up = ws.upstream("app");
    let theirs = std::sync::Mutex::new(String::new());
    let read = reader_then(2, || {
        git_env(
            &env,
            &up,
            &["commit", "-q", "--allow-empty", "-m", "theirs"],
        );
        git_env(&env, &up, &["push", "-q", "origin", "main"]);
        git_env(&env, &app, &["fetch", "-q", "origin"]);
        *theirs.lock().unwrap() = git_env(&env, &up, &["rev-parse", "HEAD"]);
    });

    let run = ws.sync_with(4, &read);

    let theirs = theirs.lock().unwrap().clone();
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    let range = format!("{theirs}..{tip}");
    ws.assert_count(&app, &[&range], 1);
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::Changed)
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), theirs);
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn a_push_url_set_after_classifying_is_refused() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("other", &[]);
    let (app, _) = ahead(&mut ws);
    let app_before = remote_refs(&ws, "app");
    let other_before = remote_refs(&ws, "other");
    let env = ws.env();
    let read = reader_then(2, || {
        git_env(
            &env,
            &app,
            &["config", "remote.origin.pushurl", "git@github.com:me/other"],
        );
    });

    let run = ws.sync_with(4, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::PushUrl)
    );
    assert_eq!(remote_refs(&ws, "app"), app_before);
    assert_eq!(remote_refs(&ws, "other"), other_before);
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn a_remote_helper_set_after_classifying_never_runs() {
    let mut ws = FixtureWorkspace::new();
    // a remote helper the caller's environment allows, which logs if run
    ws.allow_transport("fixhelper");
    let helper_log = ws.base().join("helper.log");
    write_executable(
        &ws.bin(),
        "git-remote-fixhelper",
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
            helper_log.display()
        ),
    );
    let (app, tip) = ahead(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    let env = ws.env();
    // origin's push URL stays the registry's; the transport doesn't
    let read = reader_then(2, || {
        git_env(&env, &app, &["config", "remote.origin.vcs", "fixhelper"]);
    });

    let run = ws.sync_with(4, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    // the push goes to the registry's URL over SSH, never through origin's
    // transport
    assert_eq!(
        outcome(&run, "app", "main"),
        &pushed(&remote_before["refs/heads/main"], &tip)
    );
    assert!(!helper_log.exists());
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", &tip)])
    );
    assert_eq!(ws.ssh_push_log().len(), 1);
}

#[test]
fn an_upstream_at_origin_head_is_never_pushed() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["branch", "-q", "tip"]);
    ws.git(&app, &["config", "branch.tip.remote", "origin"]);
    ws.git(&app, &["config", "branch.tip.merge", "refs/heads/HEAD"]);
    advance(&ws, &app, "tip");
    ws.assert_upstream(&app, "tip", "refs/remotes/origin/HEAD");
    ws.assert_track(&app, "tip", "[ahead 1]");
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    let reason = BranchNeedsHuman::UpstreamNotABranch;
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "tip").verdict, Verdict::NeedsHuman { reason });
    assert_eq!(
        outcome(&run, "app", "tip"),
        &BranchOutcome::NeedsHuman { reason }
    );
    // no branch named `HEAD` on the remote
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn an_archived_repo_ahead_is_left_to_a_person() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "archived = true");
    let app = ws.clone_owned("app", "app", &[]);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    let reason = BranchNeedsHuman::ArchivedAhead;
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman { reason }
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn a_busy_checkout_holds_its_push_and_no_other() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "side");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "side", "origin/side"]);
    ws.commit(&app, "local");
    let side_tip = advance(&ws, &app, "side");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.assert_track(&app, "side", "[ahead 1]");
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        app.to_str().unwrap().to_owned(),
        SessionSource::SessionFile,
    )]);

    let run = ws.sync_with(4, &|| live.clone());

    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::Busy)
    );
    // checked out nowhere: pushed
    assert_eq!(
        outcome(&run, "app", "side"),
        &pushed(&remote_before["refs/heads/side"], &side_tip)
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/side", &side_tip)])
    );
}

#[test]
fn a_session_arriving_before_the_push_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let calls = AtomicUsize::new(0);
    // none after the fetch; one in the checkout by the time sync pushes
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            LiveSessions::Known(vec![Session::at(
                child.pid(),
                child.proc_start().parse().unwrap(),
                app.to_str().unwrap().to_owned(),
                SessionSource::SessionFile,
            )])
        }
    };

    let run = ws.sync_with(4, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::Busy)
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
}

#[test]
fn a_failed_fetch_holds_the_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    // a refspec no flag confines: the fetch is refused
    ws.git(
        &app,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/main:refs/heads/mirror",
        ],
    );
    let remote_before = remote_refs(&ws, "app");
    let local_before = ws.refs(&app);

    let run = ws.sync();

    assert!(matches!(
        find_entry(&run.entries, "app").fetch_error,
        Some(RemoteFailure::RefspecOutsideOrigin { .. })
    ));
    assert_eq!(
        outcome(&run, "app", "main"),
        &held(push(1), BranchSyncHold::FetchFailed)
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.refs(&app), local_before);
    // the refused fetch never reached a remote, nor did a push
    assert_eq!(ws.ssh_log(), Vec::<String>::new());
}

#[test]
fn a_remote_refusal_fails_the_push_with_its_message() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    // what a GitHub ruleset says, from the remote's own hook
    write_executable(
        &ws.bare("app"),
        "hooks/pre-receive",
        "#!/bin/sh\necho 'error: GH006: Protected branch update failed for refs/heads/main.' >&2\n\
         echo 'error: Changes must be made through a pull request.' >&2\nexit 1\n",
    );
    let remote_before = remote_refs(&ws, "app");
    let local_before = ws.refs(&app);

    let run = ws.sync();

    let o = outcome(&run, "app", "main");
    assert_eq!(
        o,
        &BranchOutcome::PushFailed {
            failure: RemoteFailure::Rejected {
                reason: "pre-receive hook declined".into(),
                message: Some("GH006: Protected branch update failed for refs/heads/main.".into()),
            }
        }
    );
    assert!(o.failed());
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(ws.refs(&app), local_before);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), tip);
}

#[test]
fn a_configured_push_option_is_never_sent() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    // a remote that takes push options, and records what it's sent
    let bare = ws.bare("app");
    ws.git(&bare, &["config", "receive.advertisePushOptions", "true"]);
    let log = ws.base().join("push-options.log");
    write_executable(
        &bare,
        "hooks/pre-receive",
        &format!(
            "#!/bin/sh
echo \"${{GIT_PUSH_OPTION_COUNT-none}} ${{GIT_PUSH_OPTION_0-}}\" >> '{}'\n",
            log.display()
        ),
    );
    ws.git(&app, &["config", "push.pushOption", "ci.skip"]);
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &pushed(&remote_before["refs/heads/main"], &tip)
    );
    // options negotiated, none sent
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "0 \n");
}

#[test]
fn a_configured_receive_pack_never_moves_the_push() {
    let mut ws = FixtureWorkspace::new();
    // where a redirected push would land
    ws.remote("other", &[]);
    let (app, tip) = ahead(&mut ws);
    // a remote command naming another repo on the same host, the push's
    // own path commented out
    ws.git(
        &app,
        &[
            "config",
            "remote.origin.receivepack",
            "git-receive-pack 'me/other' #",
        ],
    );
    let remote_before = remote_refs(&ws, "app");
    let other_before = remote_refs(&ws, "other");

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &pushed(&remote_before["refs/heads/main"], &tip)
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", &tip)])
    );
    assert_eq!(remote_refs(&ws, "other"), other_before);
    // the command the host runs is git's own, on the registry's path
    let log = ws.ssh_push_log();
    assert_eq!(log.len(), 1, "{log:?}");
    let command = log[0].rsplit_once(" git@github.com ").unwrap().1;
    assert_eq!(command, "git-receive-pack 'me/app'");
}

#[test]
fn an_unreachable_host_fails_the_push_classified() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    // the fetch goes through, to the fixture's `ssh`; the push is denied
    write_executable(
        ws.base(),
        "denied-ssh",
        "#!/bin/sh\ncase \"$*\" in *git-upload-pack*) exec ssh \"$@\" ;; esac\n\
         echo 'git@github.com: Permission denied (publickey).' >&2\nexit 255\n",
    );
    let ssh = ws.base().join("denied-ssh");
    ws.git(&app, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::PushFailed {
            failure: RemoteFailure::Unreachable {
                cause: UnreachableCause::Auth,
                message: "git@github.com: Permission denied (publickey).".into(),
            }
        }
    );
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    // the push went through the repo's own ssh, never the one on `PATH`
    assert_eq!(ws.ssh_push_log(), Vec::<String>::new());
}

#[test]
fn a_shallow_branch_ahead_on_the_fetched_tip_is_pushed() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "main");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--depth", "1"]);
    ws.assert_shallow(&app, true);
    let tip = ws.commit(&app, "local");
    ws.write_registry();
    let remote_before = remote_refs(&ws, "app");

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &pushed(&remote_before["refs/heads/main"], &tip)
    );
    assert_eq!(
        remote_refs(&ws, "app"),
        with(&remote_before, &[("refs/heads/main", &tip)])
    );
}

/// Three repos ahead and one behind.
fn fleet() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    for name in ["a", "b", "c"] {
        let repo = ws.owned_repo(name, &[]);
        ws.commit(&repo, "local");
    }
    ws.owned_repo("d", &[]);
    ws.upstream_commit("d", "main");
    ws.write_registry();
    ws
}

#[test]
fn outcomes_are_the_same_whatever_the_jobs() {
    // two workspaces built alike: the fixture clock makes their commits
    // the same
    let one = fleet();
    let many = fleet();
    let serial = one.sync_with(1, &quiet);
    let parallel = many.sync_with(8, &quiet);
    assert_eq!(serial.outcomes, parallel.outcomes);
    for name in ["a", "b", "c"] {
        assert!(matches!(
            outcome(&serial, name, "main"),
            BranchOutcome::Pushed { .. }
        ));
        assert_eq!(
            one.git(&one.bare(name), &["rev-parse", "main"]),
            many.git(&many.bare(name), &["rev-parse", "main"])
        );
    }
    assert!(matches!(
        outcome(&serial, "d", "main"),
        BranchOutcome::FastForwarded { .. }
    ));
}
