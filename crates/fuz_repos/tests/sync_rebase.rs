//! `repos sync` rebasing a diverged registry branch: its local-only commits
//! replayed onto the fetched tip, the branch moved there — in its clean
//! checkout or in place — and pushed; and every way it's held, refused, or
//! left to a person, each followed by the exact refs, index, and working
//! tree both sides should hold (nothing else moved).
//!
//! The live-sessions reader is the seam, as in `sync.rs`: its first call
//! classifies, its second is the one right before the rebase, its third the
//! one right before the push that follows.
//!
//! Upstream history a test builds by hand reaches the bare remote by a
//! fetch into it (`publish`).

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::{BranchOutcome, BranchSyncHold, RebasePush, RebaseRefusal};
use fuz_repos::sessions::{LiveSessions, SessionSource};
use fuz_repos::state::{BranchHold, BranchNeedsHuman, Relation, SyncAction, Verdict};
use fuz_repos::{STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use support::busy::live_session;
use support::cli::{REPOS, parse, repos, stderr, stdout};
use support::push::{pushes_served, remote_refs, with};
use support::sync::outcome;
use support::{
    FixtureWorkspace, LiveChild, OWNER, assert_git_dir_unchanged, branch, files, find_entry,
    git_env, reader_then, snapshot_git_dir, write,
};

const TRACKING: &str = "refs/remotes/origin/main";
/// Origin's `HEAD`, a symbolic ref to `TRACKING`: `refs` reads it through.
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

const fn rebase(ahead: u32, behind: u32) -> SyncAction {
    SyncAction::Rebase { ahead, behind }
}

/// Moves the bare remote's `main` to the upstream author's, by a fetch into
/// it.
fn publish(ws: &FixtureWorkspace, name: &str) {
    let up = ws.upstream(name);
    ws.git(
        &ws.bare(name),
        &[
            "fetch",
            "-q",
            up.to_str().unwrap(),
            "+refs/heads/main:refs/heads/main",
        ],
    );
}

/// A clone of `app` whose `main` diverged from origin's.
struct Diverged {
    app: PathBuf,
    /// The local-only commits, oldest first.
    local: Vec<String>,
    /// Origin's tip, fetched.
    upstream: String,
}

impl Diverged {
    fn tip(&self) -> &str {
        self.local.last().unwrap()
    }
}

/// `app`'s clone with two commits on `main` and one more on origin's,
/// fetched, so it reads diverged before the tool looks; the entry's table
/// is the caller's to declare.
fn diverge(ws: &FixtureWorkspace, app: PathBuf) -> Diverged {
    let local = vec![ws.commit(&app, "local-1"), ws.commit(&app, "local-2")];
    let upstream = ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    assert_eq!(ws.git(&app, &["rev-parse", TRACKING]), upstream);
    ws.assert_track(&app, "main", "[ahead 2, behind 1]");
    ws.assert_count(&app, &["--merges", "origin/main..main"], 0);
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    Diverged {
        app,
        local,
        upstream,
    }
}

/// `app`, owned, its `main` — the registry's branch — diverged, checked
/// out and clean.
fn diverged(ws: &mut FixtureWorkspace) -> Diverged {
    let app = ws.owned_repo("app", &[]);
    let d = diverge(ws, app);
    ws.write_registry();
    d
}

/// What a run must leave untouched: the clone's refs, index, and files,
/// and the remote's refs.
#[derive(Debug, PartialEq, Eq)]
struct Untouched {
    refs: BTreeMap<String, String>,
    index: Vec<u8>,
    staged: String,
    files: Vec<(String, Vec<u8>)>,
    remote: BTreeMap<String, String>,
}

fn untouched(ws: &FixtureWorkspace, app: &Path) -> Untouched {
    Untouched {
        refs: ws.refs(app),
        index: std::fs::read(app.join(".git/index")).unwrap(),
        staged: ws.git_raw(app, &["ls-files", "--stage"]),
        files: files(app)
            .into_iter()
            .map(|f| {
                let bytes = std::fs::read(app.join(&f)).unwrap();
                (f, bytes)
            })
            .collect(),
        remote: remote_refs(ws, "app"),
    }
}

/// The run's `Rebased` outcome for `app`'s `main`: `(from, to, onto, push)`.
fn rebased(run: &fuz_repos::sync::SyncRun) -> (&str, &str, &str, &RebasePush) {
    match outcome(run, "app", "main") {
        BranchOutcome::Rebased {
            from,
            to,
            onto,
            push,
        } => (from, to, onto, push),
        other => panic!("not rebased: {other:?}"),
    }
}

/// Asserts `main` in `app` is `d`'s local commits replayed onto origin's
/// tip, `to`: the same subjects and authors, a linear chain on the fetched
/// tip, new commits.
fn assert_replayed(ws: &FixtureWorkspace, d: &Diverged, to: &str) {
    let app = &d.app;
    assert_eq!(ws.git(app, &["rev-parse", "main"]), to);
    assert_eq!(ws.git(app, &["rev-parse", "main~2"]), d.upstream);
    ws.assert_count(app, &["--merges", &format!("{}..main", d.upstream)], 0);
    let said = |rev: &str| ws.git(app, &["log", "-1", "--format=%s %an %ae %at", rev]);
    assert_eq!(said("main~1"), said(&d.local[0]));
    assert_eq!(said("main"), said(&d.local[1]));
    assert_ne!(to, d.tip());
    // the originals stay reachable, by the branch's reflog
    assert_eq!(ws.git(app, &["rev-parse", "main@{1}"]), d.tip());
}

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

    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert!(!outcome(&run, "app", "main").failed());
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
    // the checkout followed: on the branch, clean, both sides' files
    ws.assert_head(app, Some("main"));
    ws.assert_clean(app);
    assert_eq!(
        files(app),
        ["README", "local-1.txt", "local-2.txt", "upstream-main.txt"]
    );
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
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
    // the checkout, on another branch, wasn't touched
    ws.assert_head(app, Some("side"));
    assert_eq!(ws.git(app, &["rev-parse", "HEAD"]), side);
    ws.assert_clean(app);
    assert_eq!(files(app), tree_before);
}

#[test]
fn a_conflict_moves_nothing_and_leaves_it_to_a_person() {
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
    // status can't know: it predicts a rebase, and never replays
    assert_eq!(
        branch(&ws.entry("app"), "main").verdict,
        Verdict::Act {
            action: rebase(1, 1)
        }
    );
    let before = untouched(&ws, &app);

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
    assert_eq!(untouched(&ws, &app), before);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
    assert_eq!(ws.git(&app, &["rev-parse", TRACKING]), upstream);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    // no rebase left in progress
    assert!(!app.join(".git/rebase-merge").exists());
    assert!(!app.join(".git/REBASE_HEAD").exists());
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
}

#[test]
fn a_local_commit_already_upstream_is_refused_not_kept_empty() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let upstream = ws.upstream_commit("app", "main");
    // the same change, made here too (a cherry-pick's shape), between two
    // commits of its own
    let content = std::fs::read_to_string(ws.upstream("app").join("upstream-main.txt")).unwrap();
    let first = ws.commit(&app, "local-1");
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
    assert_ne!(first, same);
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

    let (from, to, onto, push) = rebased(&run);
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

#[test]
fn a_merge_among_the_local_commits_is_a_persons() {
    let mut ws = FixtureWorkspace::new();
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
    ws.assert_clean(&app);
    ws.write_registry();
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
            &BranchOutcome::Held {
                action: rebase(2, 1),
                by: BranchSyncHold::DirtyCheckout
            },
            "{dirt}"
        );
        assert_eq!(untouched(&ws, app), before, "{dirt}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new());
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
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "archived = true");
    let app = ws.clone_owned("app", "app", &[]);
    let d = diverge(&ws, app);
    ws.write_registry();
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

#[test]
fn a_busy_checkout_holds_the_rebase() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![live_session(&child, app, SessionSource::SessionFile)]);

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
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Busy
        }
    );
    assert_eq!(untouched(&ws, app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_session_arriving_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let child = LiveChild::spawn();
    let live = LiveSessions::Known(vec![live_session(&child, app, SessionSource::SessionFile)]);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    // none when classifying; one in the checkout right before acting
    let read = || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            support::quiet()
        } else {
            live.clone()
        }
    };

    let run = ws.sync_with(1, &read);

    let e = find_entry(&run.entries, "app");
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Act {
            action: rebase(2, 1)
        }
    );
    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Busy
        }
    );
    assert_eq!(untouched(&ws, app), before);
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

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
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

#[test]
fn a_branch_moved_in_place_before_the_rebase_is_held() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    ws.git(app, &["switch", "-q", "--detach", &d.upstream]);
    ws.assert_track(app, "main", "[ahead 2, behind 1]");
    let env = ws.env();
    let remote_before = remote_refs(&ws, "app");
    // another hand drops the branch's newest commit: still diverged, by
    // other counts
    let read = reader_then(2, || {
        git_env(&env, app, &["update-ref", "refs/heads/main", &d.local[0]]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
    assert_eq!(ws.git(app, &["rev-parse", "main"]), d.local[0]);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn an_upstream_that_moved_before_the_rebase_is_held() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let before = ws.refs(app);
    let root = ws.git(app, &["rev-parse", "origin/main~1"]);
    // the remote-tracking ref rewound under the verdict: the branch is
    // ahead of it now, another action's to take
    let read = reader_then(2, || {
        git_env(&env, app, &["update-ref", TRACKING, &root]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
    assert_eq!(
        ws.refs(app),
        with(&before, &[(TRACKING, &root), (ORIGIN_HEAD, &root)])
    );
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
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

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::DirtyCheckout
        }
    );
    assert_eq!(ws.refs(app), before);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), "mine\n");
    ws.assert_porcelain(app, &["?? upstream-main.txt"]);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn a_rebase_never_replaces_an_ignored_file() {
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

    let run = ws.sync();

    let BranchOutcome::Failed { action, message } = outcome(&run, "app", "main") else {
        panic!("{:?}", run.outcomes);
    };
    assert_eq!(*action, rebase(1, 1));
    assert!(
        message.contains("untracked working tree files would be overwritten"),
        "{message}"
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
fn a_remote_that_moved_after_the_fetch_holds_the_push_of_a_rebased_branch() {
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

    let run = ws.sync_with(1, &read);

    // rebased onto the tip the fetch saw; the lease refused the push
    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(
        push,
        &RebasePush::Held {
            by: BranchSyncHold::Changed
        }
    );
    assert!(!outcome(&run, "app", "main").failed());
    assert_replayed(&ws, &d, to);
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), later);
    assert_eq!(ws.git(app, &["rev-parse", TRACKING]), d.upstream);
    assert_eq!(pushes_served(&ws).len(), 1);
    ws.assert_clean(app);
    ws.assert_track(app, "main", "[ahead 2]");

    // the rerun fetches the commit, finds the branch diverged again, and
    // rebases it once more
    let rerun = ws.sync();
    let (from_again, to_again, onto_again, push) = rebased(&rerun);
    assert_eq!((from_again, onto_again), (to, later.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), to_again);
    assert_eq!(ws.git(app, &["rev-parse", "main~2"]), later);
    ws.assert_track(app, "main", "");
    ws.assert_clean(app);
}

#[test]
fn head_leaving_the_branch_before_the_rebase_is_held() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let remote_before = remote_refs(&ws, "app");
    let read = reader_then(2, || {
        git_env(&env, app, &["switch", "-q", "-c", "elsewhere"]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
    assert_eq!(ws.git(app, &["rev-parse", "main"]), d.tip());
    ws.assert_head(app, Some("elsewhere"));
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_clean(app);
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
    assert!(
        text.contains("diverged +2 −1") && text.contains("rebase"),
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
    let blog = ws.owned_repo("blog", &[]);
    let local = ws.commit(&blog, "upstream-main");
    ws.upstream_commit("blog", "main");
    ws.git(&blog, &["fetch", "-q", "origin"]);
    ws.assert_track(&blog, "main", "[ahead 1, behind 1]");
    ws.write_registry();

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
    assert_eq!(ws.git(&blog, &["rev-parse", "main"]), local);
    ws.assert_clean(&blog);
    ws.assert_track(&d.app, "main", "");
    // the conflict stands; status says what sync would try again
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with("sync would    rebase blog +1 −1\n"),
        "{text}"
    );
    // `repos push` of the conflicted branch stays a person's, unpushed
    let out = ws
        .command(REPOS, &blog)
        .env("COLUMNS", "300")
        .arg("push")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    // the hint whole, on its one line
    assert!(
        text.starts_with(
            "needs human   blog (diverged +1 −1)\n              hint: repos push never rebases \
             or force-pushes: repos sync rebases the registry's branch when its commits replay \
             cleanly; any other is resolved by hand\n"
        ),
        "{text}"
    );
    assert_eq!(ws.git(&blog, &["rev-parse", "main"]), local);
}

// --- a tag on a local-only commit: a rebase would leave it behind ---

#[test]
fn a_tag_on_a_local_commit_is_a_persons() {
    for annotated in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let d = diverged(&mut ws);
        let app = &d.app;
        // a release made here, its push refused: the commit and its tag
        if annotated {
            ws.git(app, &["tag", "-a", "-m", "v1", "v1", &d.local[0]]);
        } else {
            ws.git(app, &["tag", "v1", &d.local[0]]);
        }
        // a tag on a commit origin has holds nothing
        ws.git(app, &["tag", "base", "origin/main~1"]);
        assert_eq!(ws.git(app, &["rev-parse", "v1^{commit}"]), d.local[0]);
        ws.assert_track(app, "main", "[ahead 2, behind 1]");
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
        assert_eq!(untouched(&ws, app), before);
        assert_eq!(pushes_served(&ws), Vec::<String>::new());

        // the tag gone, sync rebases it
        ws.git(app, &["tag", "-d", "v1"]);
        let run = ws.sync();
        let (_, to, _, push) = rebased(&run);
        assert_eq!(push, &RebasePush::Pushed);
        assert_replayed(&ws, &d, to);
    }
}

#[test]
fn a_tag_landing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let remote_before = remote_refs(&ws, "app");
    let read = reader_then(2, || {
        git_env(&env, app, &["tag", "v1", d.tip()]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
    assert_eq!(ws.git(app, &["rev-parse", "main"]), d.tip());
    assert_eq!(ws.git(app, &["rev-parse", "v1"]), d.tip());
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

// --- a commit another remote branch holds is never rewritten ---

#[test]
fn a_commit_another_remote_branch_holds_is_never_rewritten() {
    for own_commit_too in [false, true] {
        let mut ws = FixtureWorkspace::new();
        ws.remote("app", &[]);
        // `feat`, pushed to origin, then merged into the local `main` by
        // fast-forward: `main` is ahead by a commit origin's `feat` holds
        let feat = ws.upstream_commit("app", "feat");
        ws.declare_repo("app", "app", "");
        let app = ws.clone_owned("app", "app", &[]);
        ws.git(&app, &["merge", "-q", "--ff-only", "origin/feat"]);
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), feat);
        let (ahead, own) = if own_commit_too {
            ws.commit(&app, "local");
            (2, 1)
        } else {
            (1, 0)
        };
        ws.upstream_commit("app", "main");
        ws.git(&app, &["fetch", "-q", "origin"]);
        ws.assert_track(&app, "main", &format!("[ahead {ahead}, behind 1]"));
        ws.assert_count(&app, &["main", "--not", "--remotes"], own);
        assert_eq!(ws.git(&app, &["rev-parse", "origin/feat"]), feat);
        ws.assert_clean(&app);
        ws.write_registry();
        let reason = BranchNeedsHuman::DivergedPublished;
        let e = ws.entry("app");
        assert_eq!(branch(&e, "main").unique_commits, own);
        assert_eq!(
            branch(&e, "main").verdict,
            Verdict::NeedsHuman { reason },
            "own commit too: {own_commit_too}"
        );
        let before = untouched(&ws, &app);

        let run = ws.sync();

        assert_eq!(
            outcome(&run, "app", "main"),
            &BranchOutcome::NeedsHuman { reason },
            "own commit too: {own_commit_too}"
        );
        assert_eq!(untouched(&ws, &app), before);
        assert_eq!(pushes_served(&ws), Vec::<String>::new());
        // origin's `feat` still holds the very commit `main` does
        assert!(
            ws.git_output(&app, &["merge-base", "--is-ancestor", &feat, "main"])
                .status
                .success()
        );
    }
}

#[test]
fn a_commit_a_remote_ref_takes_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let remote_before = remote_refs(&ws, "app");
    // another remote-tracking ref comes to hold one of the local-only
    // commits between classifying and acting (a push of it under another
    // name, recorded)
    let read = reader_then(2, || {
        git_env(
            &env,
            app,
            &["update-ref", "refs/remotes/origin/elsewhere", &d.local[0]],
        );
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::Changed
        }
    );
    assert_eq!(ws.git(app, &["rev-parse", "main"]), d.tip());
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

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
        assert!(joined.starts_with(&wanted), "{text}");
        assert_eq!(untouched(&ws, app), before);
        assert_eq!(pushes_served(&ws), Vec::<String>::new());
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
            "Configured <configured@example.com>"
        );
    }
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), to);
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

    let (from, to, _, push) = rebased(&run);
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

// --- the push that follows ---

#[test]
fn a_push_the_remote_refuses_leaves_the_branch_rebased_for_the_rerun() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let hook = ws.bare("app").join("hooks/pre-receive");
    support::write_executable(
        &ws.bare("app"),
        "hooks/pre-receive",
        "#!/bin/sh\necho 'error: GH006: Protected branch update failed.' >&2\nexit 1\n",
    );
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
            support::quiet()
        } else {
            live.clone()
        }
    };

    let run = ws.sync_with(1, &read);

    let (from, to, _, push) = rebased(&run);
    assert_eq!(from, d.tip());
    assert_eq!(
        push,
        &RebasePush::Held {
            by: BranchSyncHold::Busy
        }
    );
    assert!(!outcome(&run, "app", "main").failed());
    assert_replayed(&ws, &d, to);
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_track(app, "main", "[ahead 2]");
}

// --- what the checkout's move refuses ---

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
        assert!(
            message.contains("would be overwritten by checkout"),
            "{flag}: {message}"
        );
        assert_eq!(untouched(&ws, &app), before, "{flag}");
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
        assert_eq!(
            std::fs::read_to_string(app.join("kept.txt")).unwrap(),
            "mine, edited here\n"
        );
        assert_eq!(pushes_served(&ws), Vec::<String>::new());
    }
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

    let (from, to, onto, push) = rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::Pushed);
    assert_replayed(&ws, &d, to);
    // the worktree followed its branch
    ws.assert_head(&wt, Some("main"));
    assert_eq!(ws.git(&wt, &["rev-parse", "HEAD"]), *to);
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
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), *to);
}

// --- nothing settles a conflict: no merge driver, the user's or git's ---

#[test]
fn a_merge_driver_never_settles_a_conflict() {
    // `union`, git's own, asked for by a tracked attribute; then a driver
    // the repo's config defines, which would take one side and say so
    for driver in ["union", "mine"] {
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
        if driver == "mine" {
            let command = format!("touch '{}'; exit 0", ran.display());
            ws.git(&app, &["config", "merge.mine.driver", &command]);
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
        assert_eq!(ws.git(&app, &["rev-parse", "main"]), local);
        assert_eq!(pushes_served(&ws), Vec::<String>::new());
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
    let (_, to, _, push) = rebased(&run);
    assert_eq!(push, &RebasePush::Pushed);
    assert_eq!(
        ws.git(&app, &["show", &format!("{to}:log.txt")]),
        "a\nmine\nc\nd\ne\nf\ng\ntheirs"
    );
    ws.assert_clean(&app);
}

// --- where a push would go matters only where a push could go ---

#[test]
fn a_push_url_elsewhere_is_a_reason_only_for_a_branch_sync_would_push() {
    let elsewhere = |e: &fuz_repos::report::EntryStatus| {
        e.needs_human
            .iter()
            .any(|r| matches!(r, NeedsHuman::PushUrlMismatch { .. }))
    };
    let push_elsewhere = |ws: &FixtureWorkspace, app: &Path| {
        ws.git(
            app,
            &["config", "remote.origin.pushurl", "git@github.com:me/other"],
        );
    };

    // a diverged feature branch is a person's, pushed by no run: the push
    // URL isn't read for it, and the entry gains no reason
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "feat", "origin/feat"]);
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.commit(&app, "local");
    ws.git(&app, &["switch", "-q", "main"]);
    ws.upstream_commit("app", "feat");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "feat", "[ahead 1, behind 1]");
    ws.assert_track(&app, "main", "");
    push_elsewhere(&ws, &app);
    ws.write_registry();
    let e = ws.entry("app");
    assert_eq!(
        branch(&e, "feat").verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert!(!elsewhere(&e), "{:?}", e.needs_human);

    // the registry's branch, diverged, would be pushed: the reason, and
    // the rebase held for it
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    push_elsewhere(&ws, &d.app);
    let e = ws.entry("app");
    assert!(elsewhere(&e), "{:?}", e.needs_human);
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
        &BranchOutcome::Held {
            action: rebase(2, 1),
            by: BranchSyncHold::PushUrl
        }
    );
    assert_eq!(untouched(&ws, &d.app), before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}
