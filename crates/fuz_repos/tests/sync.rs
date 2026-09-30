//! `repos sync` over fixture workspaces: the fetch, the verdicts carried out
//! — fast-forwards in place and in clean checkouts, shallow moves — and the
//! re-checks at the moment of acting, each run followed by the exact refs,
//! HEAD, and working tree it should leave (nothing else moved).
//!
//! The live-sessions reader is the seam: sync calls it once after the
//! fetches, to classify, and again right before each action, so a reader
//! that changes the fixture on a later call stands for whatever happens
//! between classifying and acting.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::classify::NeedsHuman;
use fuz_repos::remote::RemoteFailure;
use fuz_repos::report::{BranchOutcome, BranchSync, EntrySync, FetchOutcome, SyncHold};
use fuz_repos::sessions::{LiveSessions, Session, SessionSource, Unavailable};
use fuz_repos::state::{BranchNeedsHuman, HeldBy, Relation, SyncAction, Verdict};
use fuz_repos::sync::SyncRun;
use support::{FixtureWorkspace, LiveChild, branch, find_entry, write};

const fn ff(commits: u32) -> SyncAction {
    SyncAction::FastForward { commits }
}

const fn quiet() -> LiveSessions {
    LiveSessions::Known(Vec::new())
}

fn outcomes<'a>(run: &'a SyncRun, key: &str) -> &'a EntrySync {
    run.outcomes
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no outcomes for {key}: {:?}", run.outcomes))
}

fn outcome<'a>(run: &'a SyncRun, key: &str, name: &str) -> &'a BranchOutcome {
    &outcomes(run, key)
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch {key}:{name}: {:?}", run.outcomes))
        .outcome
}

fn moved(from: &str, to: &str) -> BranchOutcome {
    BranchOutcome::FastForwarded {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// A live session of `child` working in `cwd`, as the reader vouches for it.
fn session(child: &LiveChild, cwd: &Path) -> Session {
    Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        cwd.to_str().unwrap().to_owned(),
        SessionSource::SessionFile,
    )
}

/// Runs git in `dir` under the fixture's environment `env` (a `Sync` stand-in
/// for `FixtureWorkspace::git` inside a reader), asserting success.
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
/// is the one after the fetches) first runs `then`.
fn reader_then(at: usize, then: impl Fn() + Sync) -> impl Fn() -> LiveSessions + Sync {
    let calls = AtomicUsize::new(0);
    move || {
        if calls.fetch_add(1, Ordering::SeqCst) + 1 == at {
            then();
        }
        quiet()
    }
}

/// `app`, owned, cloned with `main` checked out and a local `feat` tracking
/// `origin/feat` checked out nowhere; then upstream moves both, so after the
/// fetch each is behind by one. Returns the clone and the upstream tips.
fn behind_twice(ws: &mut FixtureWorkspace) -> (std::path::PathBuf, String, String) {
    ws.remote("app", &[("a.txt", "a\n")]);
    // a commit on each past the root, so a rewrite can leave it out
    ws.upstream_commit("app", "main");
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    let main_tip = ws.upstream_commit("app", "main");
    let feat_tip = ws.upstream_commit("app", "feat");
    // local refs don't know yet: the fetch is sync's
    ws.assert_track(&app, "main", "");
    ws.assert_track(&app, "feat", "");
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    assert_ne!(ws.git(&app, &["rev-parse", "main"]), main_tip);
    assert_ne!(ws.git(&app, &["rev-parse", "feat"]), feat_tip);
    ws.write_registry();
    (app, main_tip, feat_tip)
}

#[test]
fn fast_forwards_in_place_and_in_a_clean_checkout() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, feat_tip) = behind_twice(&mut ws);
    let before = ws.refs(&app);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::Fetched);
    assert_eq!(
        outcome(&run, "app", "main"),
        &moved(&before["refs/heads/main"], &main_tip)
    );
    assert_eq!(
        outcome(&run, "app", "feat"),
        &moved(&before["refs/heads/feat"], &feat_tip)
    );
    // exactly those two branches moved, HEAD stayed on main, the files
    // followed it, and nothing else is written
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch(
            "app",
            &before,
            &[
                ("refs/heads/main", &main_tip),
                ("refs/heads/feat", &feat_tip)
            ]
        )
    );
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    assert!(app.join("upstream-main.txt").is_file());
    // the local fetch wrote no `FETCH_HEAD` of its own: origin's is still there
    let fetch_head = std::fs::read_to_string(app.join(".git/FETCH_HEAD")).unwrap();
    // origin's, over SSH as for real
    let origin = "of github.com:me/app";
    assert!(
        !fetch_head.is_empty() && fetch_head.lines().all(|l| l.ends_with(&origin)),
        "{fetch_head}"
    );
    assert!(!run.outcomes.iter().any(|e| e.branches.is_empty()));

    // again: nothing left to do
    let again = ws.sync();
    assert_eq!(
        outcomes(&again, "app").branches,
        [
            BranchSync {
                name: "feat".into(),
                outcome: BranchOutcome::Untouched,
                repeats: None,
            },
            BranchSync {
                name: "main".into(),
                outcome: BranchOutcome::Untouched,
                repeats: None,
            },
        ]
    );
}

#[test]
fn a_dirty_checkout_holds_its_branch_and_nothing_else() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    write(&app, "scratch.txt", "mine\n");
    ws.assert_porcelain(&app, &["?? scratch.txt"]);
    let before = ws.refs(&app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::DirtyCheckout
        }
    );
    assert!(matches!(
        outcome(&run, "app", "feat"),
        BranchOutcome::FastForwarded { .. }
    ));
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)])
    );
    ws.assert_porcelain(&app, &["?? scratch.txt"]);
}

#[test]
fn a_checkout_dirtied_after_classifying_is_held() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    // a file no fast-forward touches: git's merge alone would carry it along
    let scratch = app.join("scratch.txt");
    let read = reader_then(2, || std::fs::write(&scratch, "mine\n").unwrap());

    let run = ws.sync_with(1, &read);

    // classified clean, found dirty right before the merge
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::DirtyCheckout
        }
    );
    // feat acted first (the reader's second call was its re-check)
    assert!(matches!(
        outcome(&run, "app", "feat"),
        BranchOutcome::FastForwarded { .. }
    ));
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)])
    );
    ws.assert_porcelain(&app, &["?? scratch.txt"]);
}

#[test]
fn a_busy_checkout_holds_its_branch() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![session(&child, &app)]);

    let run = ws.sync_with(4, &|| live.clone());

    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: ff(1),
            by: HeldBy::Busy
        }
    );
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Busy
        }
    );
    // a branch checked out nowhere still acts
    assert!(matches!(
        outcome(&run, "app", "feat"),
        BranchOutcome::FastForwarded { .. }
    ));
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)])
    );
    ws.assert_clean(&app);
}

#[test]
fn sessions_are_read_after_the_fetch_and_again_before_acting() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let child = LiveChild::spawn();
    let env = ws.env();
    let calls = AtomicUsize::new(0);
    // a session that appears once the fetch is done: the first read finds
    // origin already moved, so a session started during the fetch is seen
    let read = || {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            git_env(&env, &app, &["rev-parse", "refs/remotes/origin/main"]),
            main_tip,
            "read {n} came before the fetch"
        );
        LiveSessions::Known(vec![session(&child, &app)])
    };
    let run = ws.sync_with(1, &read);
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Busy
        }
    );
    assert_eq!(
        ws.git(&app, &["rev-parse", "main"]),
        before["refs/heads/main"]
    );

    // one that appears only after classifying still holds, found right
    // before the merge
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let calls = AtomicUsize::new(0);
    let late = LiveSessions::Known(vec![session(&child, &app)]);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            late.clone()
        }
    };
    let run = ws.sync_with(1, &read);
    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Busy
        }
    );
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)])
    );
    // read once to classify, once per action
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn unavailable_busy_detection_fetches_only() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, _) = behind_twice(&mut ws);
    let before = ws.refs(&app);

    let run = ws.sync_with(4, &|| LiveSessions::Unavailable(Unavailable::HomeUnknown));

    for name in ["main", "feat"] {
        assert_eq!(
            outcome(&run, "app", name),
            &BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::BusyUnknown
            },
            "{name}"
        );
    }
    // fetched, nothing else
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::Fetched);
    assert_eq!(ws.refs(&app), ws.refs_after_fetch("app", &before, &[]));
    ws.assert_clean(&app);

    // detection lost between classifying and acting holds too
    let mut ws = FixtureWorkspace::new();
    let (app, _, _) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let calls = AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            LiveSessions::Unavailable(Unavailable::HomeUnknown)
        }
    };
    let run = ws.sync_with(4, &read);
    for name in ["main", "feat"] {
        assert_eq!(
            outcome(&run, "app", name),
            &BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::BusyUnknown
            },
            "{name}"
        );
    }
    assert_eq!(ws.refs(&app), ws.refs_after_fetch("app", &before, &[]));
}

/// `app` cloned at depth 1 on every branch, `main` checked out and `side`
/// checked out nowhere, both tracking origin; upstream then moves both, and
/// sync's depth-1 fetch lands tips unconnected to the local ones.
fn shallow_behind(ws: &mut FixtureWorkspace) -> (std::path::PathBuf, String, String) {
    ws.remote("app", &[("a.txt", "a\n")]);
    ws.upstream_commit("app", "side");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--depth", "1", "--no-single-branch"]);
    ws.git(&app, &["branch", "-q", "--track", "side", "origin/side"]);
    ws.assert_shallow(&app, true);
    let main_tip = ws.upstream_commit("app", "main");
    let side_tip = ws.upstream_commit("app", "side");
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    ws.write_registry();
    (app, main_tip, side_tip)
}

#[test]
fn shallow_branches_move_in_place_and_in_a_clean_checkout() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, side_tip) = shallow_behind(&mut ws);
    let before = ws.refs(&app);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    for name in ["main", "side"] {
        assert_eq!(
            branch(e, name).verdict,
            Verdict::Act {
                action: SyncAction::Move
            },
            "{name}"
        );
    }
    let mv = |r: &str, to: &str| BranchOutcome::Moved {
        from: before[r].clone(),
        to: to.to_owned(),
    };
    assert_eq!(
        outcome(&run, "app", "main"),
        &mv("refs/heads/main", &main_tip)
    );
    assert_eq!(
        outcome(&run, "app", "side"),
        &mv("refs/heads/side", &side_tip)
    );
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch(
            "app",
            &before,
            &[
                ("refs/heads/main", &main_tip),
                ("refs/heads/side", &side_tip)
            ]
        )
    );
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    assert!(app.join("upstream-main.txt").is_file());
    // the moves kept each branch's upstream
    ws.assert_upstream(&app, "main", "refs/remotes/origin/main");
    ws.assert_upstream(&app, "side", "refs/remotes/origin/side");
}

#[test]
fn a_shallow_move_never_replaces_an_ignored_file() {
    // upstream starts tracking a path the clone ignores and holds locally
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[(".gitignore", "secret.env\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--depth", "1"]);
    let up = ws.upstream("app");
    write(&up, "secret.env", "tracked\n");
    ws.git(&up, &["add", "-f", "secret.env"]);
    ws.git(&up, &["commit", "-q", "-m", "track it"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    write(&app, "secret.env", "mine\n");
    ws.assert_clean(&app);
    ws.write_registry();
    let before = ws.refs(&app);

    let run = ws.sync();

    let BranchOutcome::Failed { action, message } = outcome(&run, "app", "main") else {
        panic!("{:?}", run.outcomes);
    };
    assert_eq!(*action, SyncAction::Move);
    assert!(
        message.contains("untracked working tree files would be overwritten"),
        "{message}"
    );
    assert_eq!(ws.refs(&app), ws.refs_after_fetch("app", &before, &[]));
    assert_eq!(
        std::fs::read_to_string(app.join("secret.env")).unwrap(),
        "mine\n"
    );
    ws.assert_clean(&app);
}

#[test]
fn a_fast_forward_never_replaces_an_ignored_file() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[(".gitignore", "secret.env\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    let up = ws.upstream("app");
    write(&up, "secret.env", "tracked\n");
    ws.git(&up, &["add", "-f", "secret.env"]);
    ws.git(&up, &["commit", "-q", "-m", "track it"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    write(&app, "secret.env", "mine\n");
    ws.assert_clean(&app);
    ws.write_registry();
    let before = ws.refs(&app);

    let run = ws.sync();

    let BranchOutcome::Failed { action, message } = outcome(&run, "app", "main") else {
        panic!("{:?}", run.outcomes);
    };
    assert_eq!(*action, ff(1));
    assert!(
        message.contains("untracked working tree files would be overwritten"),
        "{message}"
    );
    assert_eq!(ws.refs(&app), ws.refs_after_fetch("app", &before, &[]));
    assert_eq!(
        std::fs::read_to_string(app.join("secret.env")).unwrap(),
        "mine\n"
    );
}

#[test]
fn a_diverged_branch_is_left_to_a_person() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.commit(&app, "local");
    ws.upstream_commit("app", "main");
    ws.write_registry();
    let before = ws.refs(&app);

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(ws.refs(&app), ws.refs_after_fetch("app", &before, &[]));
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
}

#[test]
fn an_upstream_rewritten_before_acting_is_refused_by_git() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    // what a force-push fetched between classifying and acting would leave:
    // each upstream at a commit that doesn't contain the local branch
    let rewrite = |r: &str| {
        let tree = ws.git(&app, &["rev-parse", "HEAD:"]);
        let parent = format!("{r}~1");
        let oid = ws.git(
            &app,
            &[
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-m",
                &format!("rewritten {r}"),
            ],
        );
        // shares the branch's history, but not its tip
        ws.git_fails(&app, &["merge-base", "--is-ancestor", r, &oid]);
        oid
    };
    let feat_rewritten = rewrite("feat");
    let main_rewritten = rewrite("main");
    let env = ws.env();
    let read = reader_then(2, || {
        for (r, oid) in [("feat", &feat_rewritten), ("main", &main_rewritten)] {
            git_env(
                &env,
                &app,
                &["update-ref", &format!("refs/remotes/origin/{r}"), oid],
            );
        }
    });

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: ff(1) });
    let failed = |name| match outcome(&run, "app", name) {
        BranchOutcome::Failed { action, message } => (*action, message.clone()),
        o => panic!("{name}: {o:?}"),
    };
    // in place: the local fetch rejected it
    assert_eq!(
        failed("feat"),
        (
            ff(1),
            "git rejected moving refs/heads/feat: not a fast-forward".to_owned()
        )
    );
    // in its checkout: the merge refused
    assert_eq!(
        failed("main"),
        (
            ff(1),
            "fatal: Not possible to fast-forward, aborting.".to_owned()
        )
    );
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch(
            "app",
            &before,
            &[
                ("refs/remotes/origin/feat", &feat_rewritten),
                ("refs/remotes/origin/main", &main_rewritten),
                ("refs/remotes/origin/HEAD", &main_rewritten),
            ]
        )
    );
    assert_ne!(main_rewritten, main_tip);
    ws.assert_clean(&app);
}

#[test]
fn a_branch_checked_out_after_classifying_is_refused_in_place() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let wt = ws.outside("app-feat");
    let env = ws.env();
    let read = reader_then(2, || {
        git_env(
            &env,
            &app,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"],
        );
    });

    let run = ws.sync_with(1, &read);

    let BranchOutcome::Failed { action, message } = outcome(&run, "app", "feat") else {
        panic!("{:?}", run.outcomes);
    };
    assert_eq!(*action, ff(1));
    assert!(
        message
            .starts_with("fatal: refusing to fetch into branch 'refs/heads/feat' checked out at"),
        "{message}"
    );
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/main", &main_tip)])
    );
    ws.assert_head(&wt, Some("feat"));
    ws.assert_clean(&wt);
}

#[test]
fn references_and_pins_are_never_fetched_or_moved() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_reference("lib", "them", "lib", "");
    let lib = ws.clone_third_party("lib", "lib", &[]);
    ws.remote("pin", &[]);
    ws.declare_reference("pin", "me", "pin", "pinned = true");
    let pin = ws.clone_owned("pin", "pin", &[]);
    ws.upstream_commit("lib", "main");
    ws.upstream_commit("pin", "main");
    ws.write_registry();
    let (lib_before, pin_before) = (ws.refs(&lib), ws.refs(&pin));

    let run = ws.sync();

    for key in ["lib", "pin"] {
        let e = outcomes(&run, key);
        assert_eq!(e.fetch, FetchOutcome::NotFetched, "{key}");
        assert!(
            e.branches
                .iter()
                .all(|b| b.outcome == BranchOutcome::Untouched),
            "{key}: {e:?}"
        );
    }
    assert_eq!(ws.refs(&lib), lib_before);
    assert_eq!(ws.refs(&pin), pin_before);
}

#[test]
fn an_owned_entry_with_origin_drift_is_never_fetched() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    ws.commit(&app, "local");
    // origin names another repo, which a fetch would reach
    ws.remote("other", &[("other.txt", "other\n")]);
    ws.set_origin(&app, "other", "git@github.com:me/other");
    ws.write_registry();
    let before = ws.refs(&app);
    assert!(!app.join(".git/FETCH_HEAD").exists());
    let fetched = || {
        assert!(!app.join(".git/FETCH_HEAD").exists());
        assert_eq!(ws.refs(&app), before);
    };

    let e = find_entry(&ws.status_with_fetch(), "app").clone();
    fetched();
    assert_eq!(e.fetch_error, None);
    assert!(
        matches!(e.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
        "{:?}",
        e.needs_human
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );

    let run = ws.sync();
    fetched();
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::NotFetched);
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: SyncAction::Push { commits: 1 },
            by: SyncHold::Entry
        }
    );
    assert_eq!(ws.ssh_log(), Vec::<String>::new());

    // origin set back, it's fetched
    ws.set_origin(&app, "app", &support::owned_origin("app"));
    ws.sync();
    assert!(app.join(".git/FETCH_HEAD").exists());
}

/// Origin configured as the registry's repo, and a rewrite sending its
/// fetch to another: git fetches where the rewrite sends it, so it's never
/// fetched, and a person's, the rewrite named.
#[test]
fn an_owned_fetch_a_rewrite_sends_elsewhere_is_never_made() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    ws.commit(&app, "local");
    // a repo the rewritten fetch would reach
    ws.remote("other", &[("other.txt", "other\n")]);
    let rewrite = "url.git@github.com:me/other.insteadOf";
    ws.git(&app, &["config", rewrite, &support::owned_origin("app")]);
    assert_eq!(
        ws.git(&app, &["config", "remote.origin.url"]),
        support::owned_origin("app")
    );
    assert_eq!(
        ws.git(&app, &["ls-remote", "--get-url", "origin"]),
        "git@github.com:me/other"
    );
    ws.write_registry();
    let before = ws.refs(&app);
    let unfetched = || {
        assert!(!app.join(".git/FETCH_HEAD").exists());
        assert_eq!(ws.refs(&app), before);
    };

    let e = find_entry(&ws.status_with_fetch(), "app").clone();
    unfetched();
    assert_eq!(e.fetch_error, None);
    // the rewrite sends a push there too
    assert_eq!(
        e.needs_human,
        [
            NeedsHuman::FetchUrlMismatch {
                fetch_url: "git@github.com:me/other".into(),
                expected: support::owned_origin("app"),
                fix: None,
            },
            NeedsHuman::PushUrlMismatch {
                push_urls: vec!["git@github.com:me/other".into()],
                expected: support::owned_origin("app"),
            }
        ]
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Entry
        }
    );

    let run = ws.sync();
    unfetched();
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::NotFetched);
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: SyncAction::Push { commits: 1 },
            by: SyncHold::Entry
        }
    );
    assert_eq!(ws.ssh_log(), Vec::<String>::new());

    // the rewrite gone, it's fetched
    ws.git(&app, &["config", "--unset", rewrite]);
    ws.sync();
    assert!(app.join(".git/FETCH_HEAD").exists());
}

/// Origin spelled through an alias (`gh:me/app`) is origin drift, which
/// holds the entry; but git fetches it from the registry's repo, the
/// rewrite applied, so it's fetched, and its remote view is fresh.
#[test]
fn an_owned_origin_an_alias_resolves_to_the_registrys_repo_is_fetched() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let tip = ws.upstream_commit("app", "main");
    ws.git(&app, &["remote", "set-url", "origin", "gh:me/app"]);
    ws.git(&app, &["config", "url.git@github.com:.insteadOf", "gh:"]);
    assert_eq!(
        ws.git(&app, &["ls-remote", "--get-url", "origin"]),
        support::owned_origin("app")
    );
    ws.write_registry();
    assert_ne!(ws.git(&app, &["rev-parse", "origin/main"]), tip);

    let e = find_entry(&ws.status_with_fetch(), "app").clone();
    assert_eq!(e.fetch_error, None);
    assert_eq!(ws.git(&app, &["rev-parse", "origin/main"]), tip);
    assert!(
        matches!(e.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
        "{:?}",
        e.needs_human
    );
    assert_eq!(
        branch(&e, "main").verdict,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::Entry
        }
    );
    let log = ws.ssh_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].ends_with("git@github.com git-upload-pack 'me/app'"),
        "{log:?}"
    );

    let run = ws.sync();
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::Fetched);
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: SyncHold::Entry
        }
    );
}

#[test]
fn an_entry_that_fails_its_probe_is_refused() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.commit(&app, "local");
    // HEAD's tree, deleted: `status` fails on it
    let tree = ws.git(&app, &["rev-parse", "HEAD:"]);
    let (dir, file) = tree.split_at(2);
    std::fs::remove_file(app.join(".git/objects").join(dir).join(file)).unwrap();
    let ok = ws.owned_repo("ok", &[]);
    let ok_tip = ws.upstream_commit("ok", "main");
    ws.write_registry();
    let before = ws.refs(&app);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    assert!(e.probe_error.is_some(), "{e:?}");
    assert!(outcomes(&run, "app").branches.is_empty());
    assert_eq!(ws.refs(&app)["refs/heads/main"], before["refs/heads/main"]);
    // the other entry acts all the same
    assert_eq!(ws.git(&ok, &["rev-parse", "main"]), ok_tip);
}

#[test]
fn a_failed_fetch_holds_every_move() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, _) = behind_twice(&mut ws);
    // origin's fetch lands in refs no flag can confine: refused, not run
    ws.git(
        &app,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    );
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[behind 1]");
    let before = ws.refs(&app);

    let run = ws.sync();

    assert!(matches!(
        outcomes(&run, "app").fetch,
        FetchOutcome::Failed { .. }
    ));
    for name in ["main", "feat"] {
        assert_eq!(
            outcome(&run, "app", name),
            &BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::FetchFailed
            },
            "{name}"
        );
    }
    assert_eq!(ws.refs(&app), before);
}

#[test]
fn entries_sharing_a_repo_act_on_a_branch_once() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, feat_tip) = behind_twice(&mut ws);
    ws.git(&app, &["branch", "-q", "--track", "wt", "origin/main"]);
    // a second entry: a linked worktree of app's, on its own branch
    let wt = ws.dir("app-wt");
    ws.add_worktree(&app, &wt, &["wt"]);
    ws.declare_repo("app_wt", "app", "dir = \"app-wt\"");
    ws.write_registry();
    let before = ws.refs(&app);

    let run = ws.sync();

    // app acted on each branch for the repo; app_wt reports app's outcomes
    for (key, repeats) in [("app", None), ("app_wt", Some("app"))] {
        assert!(
            outcomes(&run, key)
                .branches
                .iter()
                .all(|b| b.repeats.as_deref() == repeats),
            "{key}: {:?}",
            run.outcomes
        );
    }
    for key in ["app", "app_wt"] {
        assert_eq!(
            outcome(&run, key, "feat"),
            &moved(&before["refs/heads/feat"], &feat_tip),
            "{key}"
        );
        assert_eq!(
            outcome(&run, key, "main"),
            &moved(&before["refs/heads/main"], &main_tip),
            "{key}"
        );
        assert_eq!(
            outcome(&run, key, "wt"),
            &moved(&before["refs/heads/wt"], &main_tip),
            "{key}"
        );
    }
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch(
            "app",
            &before,
            &[
                ("refs/heads/main", &main_tip),
                ("refs/heads/feat", &feat_tip),
                ("refs/heads/wt", &main_tip),
            ]
        )
    );
    ws.assert_clean(&app);
    ws.assert_clean(&wt);
}

#[test]
fn outcomes_are_the_same_whatever_the_jobs() {
    let build = || {
        let mut ws = FixtureWorkspace::new();
        let (_, _, _) = behind_twice(&mut ws);
        let other = ws.owned_repo("other", &[]);
        ws.commit(&other, "local");
        ws.upstream_commit("other", "main");
        let third = ws.owned_repo("third", &[]);
        ws.upstream_commit("third", "main");
        write(&third, "scratch.txt", "x\n");
        ws.write_registry();
        ws
    };
    let (one, many) = (build(), build());
    let serial = one.sync_with(1, &quiet);
    let parallel = many.sync_with(16, &quiet);
    assert_eq!(serial.outcomes, parallel.outcomes);
    let kinds: BTreeMap<&str, Vec<&BranchOutcome>> = serial
        .outcomes
        .iter()
        .map(|e| {
            (
                e.key.as_str(),
                e.branches.iter().map(|b| &b.outcome).collect(),
            )
        })
        .collect();
    assert!(matches!(
        kinds["app"][..],
        [
            BranchOutcome::FastForwarded { .. },
            BranchOutcome::FastForwarded { .. }
        ]
    ));
    assert!(matches!(
        kinds["other"][..],
        [BranchOutcome::NeedsHuman { .. }]
    ));
    assert!(matches!(
        kinds["third"][..],
        [BranchOutcome::Held {
            by: SyncHold::DirtyCheckout,
            ..
        }]
    ));
}

// --- symbolic branch refs: a write through one reaches its target unchecked ---

#[test]
fn a_symbolic_branch_ref_never_acts() {
    // `m` aliases `main`, which is checked out dirty; `m` tracks
    // `origin/main`, so it reads behind and in no checkout
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    ws.git(&app, &["symbolic-ref", "refs/heads/m", "refs/heads/main"]);
    ws.git(&app, &["config", "branch.m.remote", "origin"]);
    ws.git(&app, &["config", "branch.m.merge", "refs/heads/main"]);
    write(&app, "README", "mine\n");
    ws.assert_porcelain(&app, &[" M README"]);
    let before = ws.refs(&app);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    let m = branch(e, "m");
    assert_eq!(m.symref.as_deref(), Some("refs/heads/main"));
    assert_eq!(m.verdict, Verdict::Quiet);
    assert_eq!(m.unique_commits, 0);
    assert_eq!(outcome(&run, "app", "m"), &BranchOutcome::Untouched);
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::DirtyCheckout
        }
    );
    // `main` never moved under its dirty files; `m` is still its alias
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)])
    );
    assert_eq!(
        ws.git(&app, &["symbolic-ref", "refs/heads/m"]),
        "refs/heads/main"
    );
    ws.assert_porcelain(&app, &[" M README"]);
}

#[test]
fn a_symbolic_ref_into_remote_tracking_refs_writes_nothing() {
    // `r` aliases `origin/stale`, which upstream leaves behind `main`, and
    // tracks `origin/main`: behind, a fast-forward that would write
    // `refs/remotes/origin/stale` through it
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    let stale = ws.upstream_commit("app", "main");
    ws.git(&ws.upstream("app"), &["push", "-q", "origin", "main:stale"]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(
        &app,
        &["symbolic-ref", "refs/heads/r", "refs/remotes/origin/stale"],
    );
    ws.git(&app, &["config", "branch.r.remote", "origin"]);
    ws.git(&app, &["config", "branch.r.merge", "refs/heads/main"]);
    let main_tip = ws.upstream_commit("app", "main");
    ws.write_registry();
    let before = ws.refs(&app);
    assert_eq!(before["refs/heads/r"], stale);

    let run = ws.sync();

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "r").relation, Relation::Behind { commits: 1 });
    assert_eq!(branch(e, "r").verdict, Verdict::Quiet);
    assert_eq!(outcome(&run, "app", "r"), &BranchOutcome::Untouched);
    // `main` fast-forwarded; `origin/stale` is still origin's
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/main", &main_tip)])
    );
    assert_eq!(
        ws.git(&app, &["rev-parse", "refs/remotes/origin/stale"]),
        stale
    );
    assert_eq!(
        ws.git(&app, &["symbolic-ref", "refs/heads/r"]),
        "refs/remotes/origin/stale"
    );
}

#[test]
fn a_branch_made_a_symbolic_ref_before_acting_is_held() {
    // `feat`, classified a plain branch behind, is made an alias of `main`
    // before it acts: the fetch would write through it and move the
    // checked-out `main` under its files
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = behind_twice(&mut ws);
    let env = ws.env();
    let read = reader_then(2, || {
        git_env(
            &env,
            &app,
            &["symbolic-ref", "refs/heads/feat", "refs/heads/main"],
        );
    });

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        outcome(&run, "app", "feat"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Changed
        }
    );
    // `main` moved only by its own fast-forward, its files with it
    assert!(matches!(
        outcome(&run, "app", "main"),
        BranchOutcome::FastForwarded { .. }
    ));
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), main_tip);
    assert_eq!(
        ws.git(&app, &["symbolic-ref", "refs/heads/feat"]),
        "refs/heads/main"
    );
    ws.assert_clean(&app);
}

// --- what changed between classifying and acting holds (`changed`) ---

#[test]
fn head_leaving_the_branch_before_its_fast_forward_is_held() {
    let mut ws = FixtureWorkspace::new();
    let (app, _, feat_tip) = behind_twice(&mut ws);
    let before = ws.refs(&app);
    let env = ws.env();
    // after `feat`'s action (read 2), before `main`'s (read 3)
    let read = reader_then(3, || {
        git_env(&env, &app, &["switch", "-q", "-c", "elsewhere"]);
    });

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Changed
        }
    );
    // neither `main` nor the branch HEAD moved to was touched
    let mut expected = ws.refs_after_fetch("app", &before, &[("refs/heads/feat", &feat_tip)]);
    expected.insert(
        "refs/heads/elsewhere".into(),
        before["refs/heads/main"].clone(),
    );
    expected.insert("HEAD".into(), "refs/heads/elsewhere".into());
    assert_eq!(ws.refs(&app), expected);
    ws.assert_clean(&app);
}

#[test]
fn a_shallow_branch_that_gained_a_commit_before_its_move_is_held() {
    // one in its checkout (`main`), one in place (`side`), each given a
    // commit of its own after classifying
    let mut ws = FixtureWorkspace::new();
    let (app, _, _) = shallow_behind(&mut ws);
    let env = ws.env();
    let local = std::sync::Mutex::new(BTreeMap::new());
    let read = reader_then(2, || {
        let mut local = local.lock().unwrap();
        std::fs::write(app.join("mine.txt"), "mine\n").unwrap();
        git_env(&env, &app, &["add", "mine.txt"]);
        git_env(&env, &app, &["commit", "-q", "-m", "mine"]);
        local.insert("main", git_env(&env, &app, &["rev-parse", "main"]));
        let side = git_env(
            &env,
            &app,
            &["commit-tree", "side^{tree}", "-p", "side", "-m", "mine"],
        );
        git_env(&env, &app, &["update-ref", "refs/heads/side", &side]);
        local.insert("side", side);
    });

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    let local = local.lock().unwrap();
    for name in ["main", "side"] {
        assert_eq!(
            branch(e, name).verdict,
            Verdict::Act {
                action: SyncAction::Move
            },
            "{name}"
        );
        assert_eq!(
            outcome(&run, "app", name),
            &BranchOutcome::Held {
                action: SyncAction::Move,
                by: SyncHold::Changed
            },
            "{name}"
        );
        // the commit made after classifying is still the branch's
        assert_eq!(ws.git(&app, &["rev-parse", name]), local[name], "{name}");
    }
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
}

#[test]
fn a_branch_deleted_before_its_action_is_held() {
    // `feat` would fast-forward in place, `side` move in place; each is
    // deleted before its action
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = behind_twice(&mut ws);
    let env = ws.env();
    let read = reader_then(2, || {
        git_env(&env, &app, &["branch", "-q", "-D", "feat"]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "feat"),
        &BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Changed
        }
    );
    assert!(matches!(
        outcome(&run, "app", "main"),
        BranchOutcome::FastForwarded { .. }
    ));
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), main_tip);
    assert_eq!(ws.git(&app, &["branch", "--list", "feat"]), "");

    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = shallow_behind(&mut ws);
    let env = ws.env();
    // after `main`'s move (read 2), before `side`'s (read 3)
    let read = reader_then(3, || {
        git_env(&env, &app, &["branch", "-q", "-D", "side"]);
    });

    let run = ws.sync_with(1, &read);

    assert!(matches!(
        outcome(&run, "app", "main"),
        BranchOutcome::Moved { .. }
    ));
    assert_eq!(
        outcome(&run, "app", "side"),
        &BranchOutcome::Held {
            action: SyncAction::Move,
            by: SyncHold::Changed
        }
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), main_tip);
    assert_eq!(ws.git(&app, &["branch", "--list", "side"]), "");
    ws.assert_clean(&app);
}

#[test]
fn a_shallow_branch_checked_out_before_its_move_in_place_is_held() {
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = shallow_behind(&mut ws);
    let before = ws.refs(&app);
    let wt = ws.outside("app-side");
    let env = ws.env();
    let read = reader_then(2, || {
        git_env(
            &env,
            &app,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "side"],
        );
    });

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "side").verdict,
        Verdict::Act {
            action: SyncAction::Move
        }
    );
    assert_eq!(
        outcome(&run, "app", "side"),
        &BranchOutcome::Held {
            action: SyncAction::Move,
            by: SyncHold::Changed
        }
    );
    // `side` stayed under the worktree that took it; `main` moved
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch("app", &before, &[("refs/heads/main", &main_tip)])
    );
    ws.assert_head(&wt, Some("side"));
    ws.assert_clean(&wt);
}

#[test]
fn an_alternate_refs_command_never_runs() {
    // `app` borrows objects from a clone of its remote, and its config
    // names a command git runs to list that clone's refs during a fetch
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, feat_tip) = behind_twice(&mut ws);
    let alternate = ws.outside("alternate.git");
    ws.git(
        ws.base(),
        &[
            "clone",
            "-q",
            "--bare",
            ws.bare("app").to_str().unwrap(),
            alternate.to_str().unwrap(),
        ],
    );
    write(
        &app,
        ".git/objects/info/alternates",
        &format!("{}\n", alternate.join("objects").display()),
    );
    let marker = ws.outside("alternate-refs-ran");
    let script = ws.outside("alternate-refs.sh");
    support::write_executable(
        ws.base(),
        "alternate-refs.sh",
        &format!("#!/bin/sh\necho \"$@\" >> '{}'\n", marker.display()),
    );
    let command = format!("'{}'", script.display());
    ws.git(&app, &["config", "core.alternateRefsCommand", &command]);
    // the control: a plain fetch runs it (into a scratch ref, so sync's
    // fetch still has the work to do)
    ws.git(
        &app,
        &[
            "-c",
            "maintenance.auto=false",
            "fetch",
            "-q",
            "origin",
            "+refs/heads/main:refs/fixture/control",
        ],
    );
    assert!(marker.exists(), "control: plain git runs it");
    std::fs::remove_file(&marker).unwrap();
    ws.git(&app, &["update-ref", "-d", "refs/fixture/control"]);
    let before = ws.refs(&app);

    let run = ws.sync();

    // both fetches ran — origin's, and the local one moving `feat` in place
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::Fetched);
    assert_eq!(
        ws.refs(&app),
        ws.refs_after_fetch(
            "app",
            &before,
            &[
                ("refs/heads/main", &main_tip),
                ("refs/heads/feat", &feat_tip)
            ]
        )
    );
    assert!(
        !marker.exists(),
        "it ran: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );

    // a failed fetch in the same repo reports git's own error, not a line
    // about the command it didn't run
    ws.upstream_commit("app", "main");
    write(&app, ".git/refs/remotes/origin/main.lock", "");

    let run = ws.sync();

    let FetchOutcome::Failed {
        failure: RemoteFailure::Failed { message },
    } = &outcomes(&run, "app").fetch
    else {
        panic!("the stale lock went unreported: {:?}", run.outcomes);
    };
    assert!(message.contains("cannot lock ref"), "{message}");
    assert!(!marker.exists(), "it ran on the failed fetch");
}

#[test]
fn a_shallow_branch_made_a_symbolic_ref_before_its_move_is_held() {
    // `side`, to move in place, is made an alias of `main` after `main`
    // moved (read 3): an update would replace the alias with a plain ref
    let mut ws = FixtureWorkspace::new();
    let (app, main_tip, _) = shallow_behind(&mut ws);
    let env = ws.env();
    let read = reader_then(3, || {
        git_env(
            &env,
            &app,
            &["symbolic-ref", "refs/heads/side", "refs/heads/main"],
        );
    });

    let run = ws.sync_with(1, &read);

    assert!(matches!(
        outcome(&run, "app", "main"),
        BranchOutcome::Moved { .. }
    ));
    assert_eq!(
        outcome(&run, "app", "side"),
        &BranchOutcome::Held {
            action: SyncAction::Move,
            by: SyncHold::Changed
        }
    );
    assert_eq!(
        ws.git(&app, &["symbolic-ref", "refs/heads/side"]),
        "refs/heads/main"
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), main_tip);
    ws.assert_clean(&app);
}
