//! Structural checks over every report the goldens build: shapes a report
//! can't have whatever `classify` decides — a key twice, facts read of no
//! repo, a relation the clone's depth rules out. They restate none of
//! classify's decisions (verdicts, holds, reasons); the integration tests
//! over real git pin those.

use std::collections::BTreeSet;

use fuz_repos::busy::Sessions;
use fuz_repos::classify::NeedsHuman;
use fuz_repos::remote::RemoteFailure;
use fuz_repos::report::{EntryStatus, StatusReport};
use fuz_repos::state::{Presence, RefreshVerdict, Relation, Verdict};

/// Asserts every entry of `report` has a shape a report can have.
pub fn assert_structure(report: &StatusReport) {
    let mut keys = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    for e in &report.entries {
        // keys are the registry's, dirs claimed once (`dir_claimed_twice`)
        assert!(keys.insert(&e.key), "entry key `{}` twice", e.key);
        assert!(dirs.insert(&e.dir), "entry dir `{}` twice", e.dir);
        presence(e);
        reasons(e);
        if !matches!(report.sessions, Sessions::Available { .. }) {
            assert!(
                e.checkouts.iter().all(|c| c.busy.is_empty())
                    && e.unprobed_worktrees.iter().all(|u| u.busy.is_empty()),
                "{}: sessions placed with busy detection unavailable",
                e.key
            );
        }
        if !e.checkouts.is_empty() {
            checkouts(e);
            branches(e);
        }
    }
}

/// Owned, or a third-party reference the run refreshes: its branches are
/// compared against origin.
fn tracked(e: &EntryStatus) -> bool {
    e.writable || e.refresh == Some(RefreshVerdict::Act)
}

/// What each presence leaves in the entry.
fn presence(e: &EntryStatus) {
    let k = &e.key;
    let nothing_read = e.checkouts.is_empty()
        && e.branches.is_empty()
        && e.unprobed_worktrees.is_empty()
        && e.fetched_at.is_none()
        && e.stashes == 0;
    assert_eq!(
        e.clone.is_some(),
        e.presence == Presence::Missing,
        "{k}: a clone verdict exactly when missing"
    );
    match e.presence {
        Presence::Missing | Presence::NotARepo => assert!(
            nothing_read
                && e.layout.is_none()
                && e.refresh.is_none()
                && e.probe_error.is_none()
                && e.fetch_error.is_none(),
            "{k}: facts read of no repo"
        ),
        Presence::Present => {
            assert_eq!(
                e.checkouts.is_empty(),
                e.probe_error.is_some(),
                "{k}: checkouts read exactly when the probe didn't fail"
            );
            if e.probe_error.is_some() {
                assert!(nothing_read, "{k}: facts read past a failed probe");
            }
        }
    }
    // a fetch git ran and failed empties the primary's `FETCH_HEAD`; with no
    // other git dir to date it (no other worktree, and the common dir the
    // primary's own), the remote view's age is unknown. (`failed` is
    // anything else, the runner's own errors among them; a timeout may kill
    // git before it truncates; a refused fetch never runs.)
    let ran_and_failed = matches!(
        e.fetch_error,
        Some(
            RemoteFailure::RefGone { .. }
                | RemoteFailure::Unreachable { .. }
                | RemoteFailure::RepoNotFound { .. }
        )
    );
    let one_git_dir =
        e.checkouts.len() == 1 && !e.checkouts[0].linked && e.unprobed_worktrees.is_empty();
    if ran_and_failed && one_git_dir {
        assert!(
            e.fetched_at.is_none(),
            "{k}: dated by the `FETCH_HEAD` its failed fetch emptied"
        );
    }
}

/// Reasons only a presence can have, and one default-branch reason at most,
/// naming the branch the entry follows.
fn reasons(e: &EntryStatus) {
    let k = &e.key;
    for r in &e.needs_human {
        let missing_only = matches!(
            r,
            NeedsHuman::CloneSharesRepo { .. } | NeedsHuman::ClonedUnregistered { .. }
        );
        if missing_only {
            assert_eq!(e.presence, Presence::Missing, "{k}: {r:?}");
        }
        assert_eq!(
            matches!(r, NeedsHuman::NotARepo { .. }),
            e.presence == Presence::NotARepo,
            "{k}: {r:?} beside presence {:?}",
            e.presence
        );
    }
    let named: Vec<&String> = e
        .needs_human
        .iter()
        .filter_map(|r| match r {
            NeedsHuman::DefaultBranchMissing { branch }
            | NeedsHuman::DefaultBranchGone { branch }
            | NeedsHuman::DefaultBranchNoUpstream { branch } => Some(branch),
            _ => None,
        })
        .collect();
    assert!(named.len() <= 1, "{k}: several default-branch reasons");
    if let Some(named) = named.first() {
        assert_eq!(
            e.branch.as_ref(),
            Some(*named),
            "{k}: a default-branch reason naming a branch it doesn't follow"
        );
    }
}

fn checkouts(e: &EntryStatus) {
    let k = &e.key;
    assert!(
        e.checkouts[0].primary && e.checkouts[1..].iter().all(|c| !c.primary),
        "{k}: the primary first, alone"
    );
    let paths: BTreeSet<&str> = e.checkouts.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths.len(),
        e.checkouts.len(),
        "{k}: a checkout listed twice"
    );
}

fn branches(e: &EntryStatus) {
    let shallow = e.layout.as_ref().is_some_and(|l| l.shallow);
    for b in &e.branches {
        let k = format!("{}:{}", e.key, b.name);
        if !tracked(e) {
            // compared against no remote: local work alone, never pushed
            assert!(
                b.relation == Relation::Untracked
                    && b.unique_commits > 0
                    && b.verdict == Verdict::LocalOnly,
                "{k}: an untracked entry lists only local work"
            );
        }
        match b.relation {
            Relation::Shallow => assert!(shallow, "{k}: shallow in a full clone"),
            Relation::Behind { .. } | Relation::Diverged { .. } => {
                assert!(!shallow, "{k}: {:?} in a shallow clone", b.relation);
            }
            _ => {}
        }
    }
}
