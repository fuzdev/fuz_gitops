//! What `repos sync`'s rebase writes and refuses past its re-checks: the
//! committer it commits as, the signature it doesn't make, the reflog that
//! keeps the old commits; the replay's merges, which nothing settles — no
//! merge driver, the user's or git's — with the attributes the replayed
//! branch carries; and the checkout's move, which never replaces a file it
//! would lose. Upstream history a test builds by hand reaches the bare
//! remote by a fetch into it (`publish`).

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use fuz_repos::report::{BranchOutcome, RebasePush, RebaseRefusal};
use support::cli::{REPOS, stderr, stdout};
use support::push::pushes_served;
use support::rebase::{
    IGNORED_FILE_REFUSED, assert_replayed, diverged, ignored_file_upstream, loose_objects, publish,
    rebase, sync_rebased, untouched,
};
use support::sync::outcome;
use support::{FixtureWorkspace, write, write_executable};

// --- the committer is an identity the user set, never git's guess ---

/// `repos sync` with no identity in the environment: what the repo's own
/// config sets is all git has.
fn sync_without_identity(ws: &FixtureWorkspace) -> std::process::Output {
    let mut cmd = ws.command(REPOS, &ws.root());
    for var in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        cmd.env_remove(var);
    }
    cmd.arg("sync").output().unwrap()
}

#[test]
fn without_an_identity_the_rebase_fails_saying_what_to_set() {
    // none at all, then half of one: git guesses neither
    for (configured, missing) in [(None, "email"), (Some("user.email"), "name")] {
        let mut ws = FixtureWorkspace::new();
        let d = diverged(&mut ws);
        let app = &d.app;
        if let Some(key) = configured {
            ws.git(app, &["config", key, "me@example.com"]);
        }
        let before = untouched(&ws, app);

        let out = sync_without_identity(&ws);

        assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
        let text = stdout(&out);
        let said = format!(
            "failed        app (rebase: fatal: no {missing} was given and auto-detection is \
             disabled — a rebase commits as you: set user.name and user.email"
        );
        let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let wanted = said.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(joined.starts_with(&wanted), "{missing}: {text}");
        assert_eq!(untouched(&ws, app), before, "{missing}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{missing}");
        ws.assert_clean(app);
    }
}

#[test]
fn a_configured_identity_commits_the_replay() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    ws.git(app, &["config", "user.name", "Configured"]);
    ws.git(app, &["config", "user.email", "configured@example.com"]);

    let out = sync_without_identity(&ws);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let to = ws.git(app, &["rev-parse", "main"]);
    assert_replayed(&ws, &d, &to);
    for rev in ["main", "main~1"] {
        assert_eq!(
            ws.git(app, &["log", "-1", "--format=%cn <%ce>", rev]),
            "Configured <configured@example.com>",
            "{rev}"
        );
    }
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
}

#[test]
fn a_replay_signs_nothing_whatever_the_config() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // signing asked for, through a program that says it ran and fails
    let ran = ws.base().join("gpg-ran");
    write_executable(
        ws.base(),
        "gpg",
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
    );
    ws.git(app, &["config", "commit.gpgSign", "true"]);
    let gpg = ws.base().join("gpg");
    ws.git(app, &["config", "gpg.program", gpg.to_str().unwrap()]);

    let run = ws.sync();

    let (_, to, _, push) = sync_rebased(&run);
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    assert!(!ran.exists(), "the gpg program ran");
    for rev in ["main", "main~1"] {
        let commit = ws.git(app, &["cat-file", "commit", rev]);
        assert!(!commit.contains("gpgsig"), "{rev}: {commit}");
    }
    // the config takes: a commit by hand runs the program, and fails with it
    let by_hand = ws.git_output(app, &["commit", "-q", "--allow-empty", "-m", "by hand"]);
    assert!(!by_hand.status.success());
    assert!(ran.exists(), "git ran no gpg program by hand");
}

// --- the old commits stay in the reflog ---

#[test]
fn an_in_place_rebase_writes_the_reflog_whatever_the_config() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    // reflogs off, and none kept for the branch: with nothing written, the
    // replaced commits would be unreachable
    ws.git(app, &["config", "core.logAllRefUpdates", "false"]);
    ws.git(app, &["switch", "-q", "--detach", &d.upstream]);
    std::fs::remove_file(app.join(".git/logs/refs/heads/main")).unwrap();
    assert_eq!(ws.git_raw(app, &["reflog", "show", "refs/heads/main"]), "");
    ws.assert_track(app, "main", "[ahead 2, behind 1]");

    let run = ws.sync();

    let (from, to, _, push) = sync_rebased(&run);
    assert_eq!(from, d.tip());
    assert_eq!(push, &RebasePush::Pushed);
    // `main@{1}` among them: the commit replaced
    assert_replayed(&ws, &d, to);
    assert_eq!(
        ws.git(
            app,
            &["log", "-g", "-1", "--format=%gs", "refs/heads/main", "--"]
        ),
        "repos: rebase onto the fetched tip"
    );
}

// --- what the checkout's move refuses ---

#[test]
fn a_rebase_never_replaces_an_ignored_file() {
    let mut ws = FixtureWorkspace::new();
    let (app, local) = ignored_file_upstream(&mut ws);
    let before = untouched(&ws, &app);
    let objects = loose_objects(&ws, &app);

    let run = ws.sync();

    let BranchOutcome::Failed { action, message } = outcome(&run, "app", "main") else {
        panic!("{:?}", run.outcomes);
    };
    assert_eq!(*action, rebase(1, 1));
    assert_eq!(message, IGNORED_FILE_REFUSED);
    // the replay ran — its commits written, unreachable — and nothing
    // moved: the branch, the index, the file
    assert_ne!(loose_objects(&ws, &app), objects);
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
fn a_file_hidden_from_status_that_the_upstream_changes_is_never_overwritten() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let mut ws = FixtureWorkspace::new();
        ws.remote("app", &[("kept.txt", "base\n")]);
        ws.declare_repo("app", "app", "");
        let app = ws.clone_owned("app", "app", &[]);
        let local = ws.commit(&app, "local");
        // edited here, and hidden from status — to another length, so git
        // sees the edit by the file's size and the test doesn't rest on how
        // git tells a same-size edit made in the second the index was
        // written
        ws.git(&app, &["update-index", flag, "kept.txt"]);
        write(&app, "kept.txt", "mine, edited here\n");
        // the upstream changes the same file
        let up = ws.upstream("app");
        write(&up, "kept.txt", "theirs\n");
        ws.git(&up, &["commit", "-q", "-a", "-m", "change it"]);
        publish(&ws, "app");
        ws.git(&app, &["fetch", "-q", "origin"]);
        ws.assert_track(&app, "main", "[ahead 1, behind 1]");
        ws.assert_clean(&app);
        ws.write_registry();
        let before = untouched(&ws, &app);

        let run = ws.sync();

        let BranchOutcome::Failed { action, message } = outcome(&run, "app", "main") else {
            panic!("{flag}: {:?}", run.outcomes);
        };
        assert_eq!(*action, rebase(1, 1), "{flag}");
        assert_eq!(
            message,
            "error: Your local changes to the following files would be overwritten by \
             checkout: kept.txt",
            "{flag}"
        );
        assert_eq!(untouched(&ws, &app), before, "{flag}");
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), local, "{flag}");
        assert_eq!(
            std::fs::read_to_string(app.join("kept.txt")).unwrap(),
            "mine, edited here\n",
            "{flag}"
        );
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{flag}");
    }
}

// --- nothing settles a conflict: no merge driver, the user's or git's ---

#[test]
fn a_merge_driver_never_settles_a_conflict() {
    // `union`, git's own, asked for by a tracked attribute; a driver the
    // repo's config defines, which would take one side and say so; and one
    // whose name holds an `=`, which a `-c` override would split at
    for driver in ["union", "mine", "a=b"] {
        let mut ws = FixtureWorkspace::new();
        ws.remote(
            "app",
            &[
                ("log.txt", "a\nb\nc\n"),
                (".gitattributes", &format!("log.txt merge={driver}\n")),
            ],
        );
        ws.declare_repo("app", "app", "");
        let app = ws.clone_owned("app", "app", &[]);
        let ran = ws.base().join("driver-ran");
        if driver != "union" {
            let command = format!("touch '{}'; exit 0", ran.display());
            ws.git(
                &app,
                &["config", &format!("merge.{driver}.driver"), &command],
            );
        }
        // both sides change the same line
        write(&app, "log.txt", "a\nmine\nc\n");
        ws.git(&app, &["commit", "-q", "-a", "-m", "mine"]);
        let local = ws.git(&app, &["rev-parse", "HEAD"]);
        let up = ws.upstream("app");
        write(&up, "log.txt", "a\ntheirs\nc\n");
        ws.git(&up, &["commit", "-q", "-a", "-m", "theirs"]);
        publish(&ws, "app");
        ws.git(&app, &["fetch", "-q", "origin"]);
        ws.assert_track(&app, "main", "[ahead 1, behind 1]");
        ws.assert_clean(&app);
        // as git would merge it by hand, the driver settles it
        let by_hand = ws.git_output(&app, &["merge-tree", "--write-tree", "main", "origin/main"]);
        assert!(by_hand.status.success(), "{driver}: git itself conflicts");
        if driver != "union" {
            assert!(ran.exists(), "{driver}: git ran no driver by hand");
        }
        let _ = std::fs::remove_file(&ran);
        ws.write_registry();
        let before = untouched(&ws, &app);

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &BranchOutcome::RebaseRefused {
                why: RebaseRefusal::Conflicts
            },
            "{driver}"
        );
        assert!(!ran.exists(), "{driver}: the configured driver ran");
        assert_eq!(untouched(&ws, &app), before, "{driver}");
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), local, "{driver}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{driver}");
    }
}

#[test]
fn a_path_the_branch_marks_unmergeable_conflicts_wherever_the_replay_runs() {
    // the replay runs in the primary checkout, whatever branch that's on:
    // the attributes must be the replayed branch's own
    for layout in ["checked out", "in place", "linked"] {
        let mut ws = FixtureWorkspace::new();
        ws.remote("app", &[("lock.txt", "a\nb\nc\nd\ne\nf\ng\nh\n")]);
        ws.declare_repo("app", "app", "");
        let app = ws.clone_owned("app", "app", &[]);
        // here: the path marked unmergeable, and its second line changed
        write(&app, ".gitattributes", "lock.txt -merge\n");
        write(&app, "lock.txt", "a\nmine\nc\nd\ne\nf\ng\nh\n");
        ws.git(&app, &["add", "-A"]);
        ws.git(&app, &["commit", "-q", "-m", "mine"]);
        let local = ws.git(&app, &["rev-parse", "HEAD"]);
        // there: its last line, no overlap — a text merge would take both
        let up = ws.upstream("app");
        write(&up, "lock.txt", "a\nb\nc\nd\ne\nf\ng\ntheirs\n");
        ws.git(&up, &["commit", "-q", "-a", "-m", "theirs"]);
        publish(&ws, "app");
        ws.git(&app, &["fetch", "-q", "origin"]);
        match layout {
            "checked out" => {}
            "in place" => {
                ws.git(&app, &["switch", "-q", "--detach", "origin/main"]);
            }
            _ => {
                ws.git(&app, &["switch", "-q", "-c", "side", "origin/main"]);
                let wt = ws.outside("app-main");
                ws.add_worktree(&app, &wt, &["main"]);
                ws.assert_head(&wt, Some("main"));
                ws.assert_clean(&wt);
            }
        }
        ws.assert_track(&app, "main", "[ahead 1, behind 1]");
        ws.assert_clean(&app);
        assert_eq!(
            app.join(".gitattributes").exists(),
            layout == "checked out",
            "{layout}: the primary's attributes"
        );
        ws.write_registry();

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &BranchOutcome::RebaseRefused {
                why: RebaseRefusal::Conflicts
            },
            "{layout}"
        );
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), local, "{layout}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{layout}");
    }
}

#[test]
fn a_default_merge_driver_never_stands_in_for_the_text_merge() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("log.txt", "a\nb\nc\nd\ne\nf\ng\nh\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    // `union` for every path with no `merge` attribute: overridden for the
    // replay (unpinned, the replay would take it, and — `union` made to
    // fail — conflict)
    ws.git(&app, &["config", "merge.default", "union"]);
    write(&app, "log.txt", "a\nmine\nc\nd\ne\nf\ng\nh\n");
    ws.git(&app, &["commit", "-q", "-a", "-m", "mine"]);
    let up = ws.upstream("app");
    write(&up, "log.txt", "a\nb\nc\nd\ne\nf\ng\ntheirs\n");
    ws.git(&up, &["commit", "-q", "-a", "-m", "theirs"]);
    publish(&ws, "app");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    ws.assert_clean(&app);
    ws.write_registry();

    let run = ws.sync();

    // no overlap: the plain three-way text merge takes both
    let (_, to, _, push) = sync_rebased(&run);
    assert_eq!(push, &RebasePush::Pushed);
    assert_eq!(
        ws.git(&app, &["show", &format!("{to}:log.txt")]),
        "a\nmine\nc\nd\ne\nf\ng\ntheirs"
    );
    ws.assert_clean(&app);
}
