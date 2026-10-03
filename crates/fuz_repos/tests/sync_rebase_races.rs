//! `repos sync`'s rebase held by what changes between classifying and
//! acting: a session arriving, a commit, a tag, or a remote-tracking ref
//! landing, the branch or its upstream moved, the checkout dirtied or
//! switched away — each found by the rebase's re-checks, before the replay
//! writes anything — and the push after a rebase held or found done by the
//! remote moving under it.
//!
//! The live-sessions reader is the seam, as in `sync.rs`: its first call
//! classifies, its second is the one right before the rebase, its third the
//! one right before the push that follows (`reader_then`,
//! `arriving_after`).

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use fuz_repos::report::{BranchOutcome, BranchSyncHold, RebasePush};
use fuz_repos::state::Verdict;
use support::busy::live_in;
use support::push::{pushes_served, remote_refs, with};
use support::rebase::{
    assert_replayed, diverged, loose_objects, rebase, rebase_held, refs_rebased, sync_rebased,
    untouched,
};
use support::sync::outcome;
use support::{
    FixtureWorkspace, LiveChild, ORIGIN_HEAD, TRACKING, arriving_after, branch, find_entry,
    git_env, reader_then,
};

// --- held right before the rebase ---

#[test]
fn a_session_arriving_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = untouched(&ws, app);
    let child = LiveChild::spawn();
    let live = live_in(&child, app);
    // none when classifying; one in the checkout right before acting
    let read = arriving_after(1, live);

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
        &rebase_held(BranchSyncHold::Busy)
    );
    assert_eq!(untouched(&ws, app), before);
}

#[test]
fn a_commit_landing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let before = ws.refs(app);
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
        &rebase_held(BranchSyncHold::Changed)
    );
    // the commit that landed is the branch's tip still, on the commit
    // classified: never replayed around, never dropped
    let landed = landed.lock().unwrap().clone();
    assert_eq!(ws.refs(app), with(&before, &[("refs/heads/main", &landed)]));
    assert_eq!(ws.git(app, &["rev-parse", "main~1"]), d.tip());
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
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");
    // another hand drops the branch's newest commit: still diverged, by
    // other counts
    let read = reader_then(2, || {
        git_env(&env, app, &["update-ref", "refs/heads/main", &d.local[0]]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::Changed)
    );
    assert_eq!(
        ws.refs(app),
        with(&before, &[("refs/heads/main", &d.local[0])])
    );
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
        &rebase_held(BranchSyncHold::Changed)
    );
    assert_eq!(
        ws.refs(app),
        with(&before, &[(TRACKING, &root), (ORIGIN_HEAD, &root)])
    );
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

#[test]
fn a_tag_landing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");
    let read = reader_then(2, || {
        git_env(&env, app, &["tag", "v1", d.tip()]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::Changed)
    );
    assert_eq!(ws.refs(app), with(&before, &[("refs/tags/v1", d.tip())]));
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

#[test]
fn a_commit_a_remote_ref_takes_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let before = ws.refs(app);
    let remote_before = remote_refs(&ws, "app");
    let elsewhere = "refs/remotes/origin/elsewhere";
    // another remote-tracking ref comes to hold one of the local-only
    // commits between classifying and acting (a push of it under another
    // name, recorded)
    let read = reader_then(2, || {
        git_env(&env, app, &["update-ref", elsewhere, &d.local[0]]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::Changed)
    );
    assert_eq!(ws.refs(app), with(&before, &[(elsewhere, &d.local[0])]));
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
    ws.assert_clean(app);
}

// --- the checkout dirtied or switched away ---

#[test]
fn a_file_appearing_before_the_rebase_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let before = ws.refs(app);
    let objects = loose_objects(&ws, app);
    // the very path the upstream's commit adds, untracked here
    let scratch = app.join("upstream-main.txt");
    let read = reader_then(2, || std::fs::write(&scratch, "mine\n").unwrap());

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::DirtyCheckout)
    );
    assert_eq!(ws.refs(app), before);
    // held before the replay ran: no commit written
    assert_eq!(loose_objects(&ws, app), objects);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), "mine\n");
    ws.assert_porcelain(app, &["?? upstream-main.txt"]);
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn head_leaving_the_branch_before_the_rebase_is_held() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let env = ws.env();
    let before = ws.refs(app);
    let objects = loose_objects(&ws, app);
    let remote_before = remote_refs(&ws, "app");
    let read = reader_then(2, || {
        git_env(&env, app, &["switch", "-q", "-c", "elsewhere"]);
    });

    let run = ws.sync_with(1, &read);

    assert_eq!(
        outcome(&run, "app", "main"),
        &rebase_held(BranchSyncHold::Changed)
    );
    // the other hand's branch and switch, and nothing of the tool's
    assert_eq!(
        ws.refs(app),
        with(
            &before,
            &[
                ("refs/heads/elsewhere", d.tip()),
                ("HEAD", "refs/heads/elsewhere")
            ]
        )
    );
    assert_eq!(loose_objects(&ws, app), objects);
    ws.assert_head(app, Some("elsewhere"));
    assert_eq!(remote_refs(&ws, "app"), remote_before);
    ws.assert_clean(app);
}

// --- the push after a rebase, raced ---

#[test]
fn a_session_arriving_before_the_push_holds_it_rebased() {
    let mut ws = FixtureWorkspace::new();
    let d = diverged(&mut ws);
    let app = &d.app;
    let remote_before = remote_refs(&ws, "app");
    let child = LiveChild::spawn();
    let live = live_in(&child, app);
    // none when classifying or rebasing; one in the checkout right before
    // the push
    let read = arriving_after(2, live);

    let run = ws.sync_with(1, &read);

    let (from, to, _, push) = sync_rebased(&run);
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
    let (from, to, onto, push) = sync_rebased(&run);
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
    let (from_again, to_again, onto_again, push) = sync_rebased(&rerun);
    assert_eq!((from_again, onto_again), (to, later.as_str()));
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
    // the replayed tip itself reaches origin after the rebase, right before
    // the push, by a fetch into the remote
    let read = reader_then(3, || {
        git_env(
            &env,
            &bare,
            &["fetch", "-q", &app_path, "+refs/heads/main:refs/heads/main"],
        );
    });

    let run = ws.sync_with(1, &read);

    // rebased, and nothing left to send: no failure
    let (from, to, onto, push) = sync_rebased(&run);
    assert_eq!((from, onto), (d.tip(), d.upstream.as_str()));
    assert_eq!(push, &RebasePush::AlreadyThere);
    assert!(!outcome(&run, "app", "main").failed());
    assert_replayed(&ws, &d, to);
    // the remote, the branch, and the remote-tracking ref the push recorded
    // all hold the replayed tip
    assert_eq!(ws.git(&bare, &["rev-parse", "main"]), to);
    assert_eq!(ws.refs(app), refs_rebased(&before, to));
    // the remote was asked, under the lease, and had it already
    assert_eq!(pushes_served(&ws), ["git-receive-pack 'me/app'"]);
    ws.assert_clean(app);
    ws.assert_track(app, "main", "");
    let again = ws.sync();
    assert_eq!(outcome(&again, "app", "main"), &BranchOutcome::Untouched);
}
