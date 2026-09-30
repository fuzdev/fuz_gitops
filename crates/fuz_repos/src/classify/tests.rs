use std::collections::HashSet;
use std::path::PathBuf;

use super::*;
use crate::porcelain::{ConfigFacts, OriginUrl, RefFacts, StatusFacts};
use crate::registry::EntryKind;
use crate::sessions::{Session, SessionSource};
use crate::state::{Checkout, GitDirHolds, Layout, Uncommitted, UnprobedWhy, UnprobedWorktree};

const NOW: u64 = 1_800_000_000;

fn url(s: &str) -> RepoUrl {
    RepoUrl::try_from(s.to_owned()).unwrap()
}

/// Where a test entry's checkout lives and who moves its HEAD.
#[derive(Debug, Clone, Copy)]
enum Mode<'a> {
    Follow(&'a str),
    Pinned,
    /// Pinned, its checkout living on the branch.
    PinnedOn(&'a str),
    Head,
}

fn owned(mode: Mode<'_>) -> Entry {
    let (branch, pinned) = match mode {
        Mode::Follow(b) => (Some(b.to_owned()), false),
        Mode::Pinned => (None, true),
        Mode::PinnedOn(b) => (Some(b.to_owned()), true),
        Mode::Head => (None, false),
    };
    Entry {
        key: "app".into(),
        kind: EntryKind::Repo,
        dir: "app".into(),
        url: url("https://github.com/me/app"),
        writable: true,
        archived: false,
        visibility: None,
        ci: false,
        branch,
        pinned,
        shallow: false,
        sparse: None,
        same_repo_as: None,
    }
}

fn third_party(mode: Mode<'_>) -> Entry {
    Entry {
        url: url("https://github.com/them/lib"),
        writable: false,
        kind: EntryKind::Reference,
        ..owned(mode)
    }
}

/// A branch: name, configured upstream remote (`None` = no config), the
/// resolved upstream, the track, unique commits.
struct B<'a> {
    name: &'a str,
    remote: Option<&'a str>,
    resolved: bool,
    track: Track,
    unique: u32,
    on_tip: bool,
}

const fn b<'a>(name: &'a str, remote: Option<&'a str>, resolved: bool, track: Track) -> B<'a> {
    B {
        name,
        remote,
        resolved,
        track,
        unique: 0,
        on_tip: false,
    }
}

impl B<'_> {
    const fn unique(mut self, n: u32) -> Self {
        self.unique = n;
        self
    }
    const fn on_tip(mut self) -> Self {
        self.on_tip = true;
        self
    }
}

fn facts(head: Head, branches: &[B<'_>]) -> RepoFacts {
    let mut config = ConfigFacts {
        origin_urls: vec![OriginUrl::repo("git@github.com:me/app")],
        origin_keys: OriginKeys::InRepo,
        ..ConfigFacts::default()
    };
    for b in branches {
        if let Some(remote) = b.remote {
            config.branches.insert(
                b.name.into(),
                BranchConfig {
                    remote: Some(remote.into()),
                    merge: Some(format!("refs/heads/{}", b.name)),
                },
            );
        }
    }
    RepoFacts {
        path: "/ws/app".into(),
        common_dir: PathBuf::from("/ws/app/.git"),
        repo_key: PathBuf::from("/ws/app/.git"),
        config,
        status: StatusFacts {
            head,
            uncommitted: Uncommitted::default(),
            stashes: 0,
        },
        in_progress: None,
        primary_linked: false,
        primary_locked: false,
        locks: Vec::new(),
        worktrees: Vec::new(),
        registry_worktrees: HashSet::new(),
        unprobed: Vec::new(),
        relative_gitdir: None,
        unreadable: Vec::new(),
        git_dirs: Vec::new(),
        bare_main: None,
        branches: branches
            .iter()
            .map(|b| BranchFacts {
                branch: RefFacts {
                    name: b.name.into(),
                    oid: format!("c-{}", b.name),
                    symref: None,
                    upstream_ref: b.resolved.then(|| {
                        format!("refs/remotes/{}/{}", b.remote.unwrap_or("origin"), b.name)
                    }),
                    merge_ref: b.resolved.then(|| format!("refs/heads/{}", b.name)),
                    track: b.track,
                    worktree: None,
                    committer_time: NOW - 3600,
                },
                unique_commits: b.unique,
                on_fetched_tip: b.on_tip,
            })
            .collect(),
        layout: Layout {
            shallow: false,
            sparse: false,
            partial_filter: None,
        },
        fetched_at: None,
        fetch_failed: false,
        push_urls: Some(vec!["git@github.com:me/app".into()]),
    }
}

fn on(name: &str) -> Head {
    Head::Branch { name: name.into() }
}

fn relations(entry: &Entry, f: &RepoFacts) -> Vec<(String, Relation)> {
    classify(entry, f, &EntrySessions::idle(), Refresh::Unasked)
        .branches
        .into_iter()
        .map(|b| (b.name, b.relation))
        .collect()
}

const O: Option<&str> = Some("origin");

#[test]
fn owned_relations_from_the_track() {
    let f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("ahead", O, true, Track::Ahead(2)).unique(2),
            b("behind", O, true, Track::Behind(3)),
            b(
                "diverged",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 4,
                },
            )
            .unique(1),
            b("gone", O, true, Track::Gone).unique(1),
            b("unmapped", O, false, Track::Even).unique(5),
            b("local", None, false, Track::Even).unique(1),
            b("merged", None, false, Track::Even),
            b("other", Some("upstream"), true, Track::Behind(9)),
        ],
    );
    let got = relations(&owned(Mode::Follow("main")), &f);
    let want = [
        ("main", Relation::InSync),
        ("ahead", Relation::Ahead { commits: 2 }),
        ("behind", Relation::Behind { commits: 3 }),
        (
            "diverged",
            Relation::Diverged {
                ahead: 1,
                behind: 4,
            },
        ),
        ("gone", Relation::Gone),
        ("unmapped", Relation::Unmapped),
        ("local", Relation::Untracked),
        ("merged", Relation::Untracked),
        ("other", Relation::Untracked),
    ];
    let want: Vec<_> = want.iter().map(|(n, r)| ((*n).to_owned(), *r)).collect();
    assert_eq!(got, want);
}

fn verdicts(entry: &Entry, f: &RepoFacts) -> Vec<(String, Verdict)> {
    classify(entry, f, &EntrySessions::idle(), Refresh::Unasked)
        .branches
        .into_iter()
        .map(|b| (b.name, b.verdict))
        .collect()
}

fn named(want: &[(&str, Verdict)]) -> Vec<(String, Verdict)> {
    want.iter()
        .map(|(n, v)| ((*n).to_owned(), v.clone()))
        .collect()
}

const fn act(action: SyncAction) -> Verdict {
    Verdict::Act { action }
}

const fn needs(reason: BranchNeedsHuman) -> Verdict {
    Verdict::NeedsHuman { reason }
}

#[test]
fn owned_verdicts() {
    let mut f = facts(
        on("fresh"),
        &[
            b("main", None, false, Track::Even),
            b("ahead", O, true, Track::Ahead(2)).unique(2),
            b("behind", O, true, Track::Behind(3)),
            b(
                "diverged",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 4,
                },
            )
            .unique(1),
            b("gone", O, true, Track::Gone).unique(1),
            b("unmapped", O, false, Track::Even).unique(5),
            b("local", None, false, Track::Even).unique(1),
            b("merged", None, false, Track::Even),
            b("other", Some("upstream"), true, Track::Behind(9)),
            b("fresh", None, false, Track::Even),
        ],
    );
    f.branches[9].branch.worktree = Some("/ws/app".into());
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[
            // the registry's branch without an upstream is an entry
            // reason, not merged work
            ("main", Verdict::Quiet),
            ("ahead", act(SyncAction::Push { commits: 2 })),
            ("behind", act(SyncAction::FastForward { commits: 3 })),
            ("diverged", needs(BranchNeedsHuman::Diverged)),
            (
                "gone",
                Verdict::Cleanup {
                    reason: CleanupReason::UpstreamGone,
                    removable_worktree: None
                }
            ),
            ("unmapped", needs(BranchNeedsHuman::Unmapped)),
            ("local", Verdict::LocalOnly),
            (
                "merged",
                Verdict::Cleanup {
                    reason: CleanupReason::Merged,
                    removable_worktree: None
                }
            ),
            ("other", Verdict::Quiet),
            // checked out with nothing committed: a fresh branch
            ("fresh", Verdict::Quiet),
        ])
    );
}

#[test]
fn entry_reasons_hold_every_action() {
    let branches = [
        b("main", O, true, Track::Ahead(1)).unique(1),
        b("feat", O, true, Track::Behind(2)),
        b("wip", None, false, Track::Even).unique(1),
    ];
    let held = [
        (
            "main",
            Verdict::Held {
                action: SyncAction::Push { commits: 1 },
                by: HeldBy::Entry,
            },
        ),
        (
            "feat",
            Verdict::Held {
                action: SyncAction::FastForward { commits: 2 },
                by: HeldBy::Entry,
            },
        ),
        // not an action, so not held
        ("wip", Verdict::LocalOnly),
    ];
    let e = owned(Mode::Follow("main"));

    let mut drift = facts(on("main"), &branches);
    drift.config.origin_urls = vec![OriginUrl::repo("git@github.com:someone/app")];
    assert_eq!(verdicts(&e, &drift), named(&held));

    let mut rebasing = facts(on("main"), &branches);
    rebasing.in_progress = Some(InProgressOp::Rebase);
    assert_eq!(verdicts(&e, &rebasing), named(&held));

    // a branch-scoped reason leaves the other branches to sync
    let detached = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &branches,
    );
    assert_eq!(
        verdicts(&e, &detached)[..2],
        named(&[
            ("main", act(SyncAction::Push { commits: 1 })),
            ("feat", act(SyncAction::FastForward { commits: 2 })),
        ])
    );
}

/// A linked worktree at `path`, on `head`, clean.
fn linked(path: &str, head: Head) -> Checkout {
    Checkout {
        path: path.into(),
        primary: false,
        head,
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: Some(false),
        busy: Vec::new(),
        working: Vec::new(),
    }
}

/// An unprobed worktree at `path`, on `branch`.
fn unprobed(path: &str, branch: Option<&str>, why: UnprobedWhy) -> UnprobedWorktree {
    UnprobedWorktree {
        path: path.into(),
        git_dir: Some("/ws/app/.git/worktrees/wt".into()),
        head: branch.map_or_else(
            || UnprobedHead::Detached {
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            |name| UnprobedHead::Branch {
                name: name.to_owned(),
            },
        ),
        locked: false,
        in_progress: None,
        why,
        holds: None,
    }
}

#[test]
fn a_dirty_checkout_holds_all_but_a_push() {
    let ff = |commits| SyncAction::FastForward { commits };
    let held = |action, by| Verdict::Held { action, by };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(2)),
            b("other", O, true, Track::Behind(3)),
            b("linked", O, true, Track::Behind(1)),
            b("linked-ahead", O, true, Track::Ahead(1)).unique(1),
        ],
    );
    // `%(worktreepath)` is git's resolved path; the checkouts are matched
    // by the branch on their HEAD, never by path
    f.branches[0].branch.worktree = Some("/real/ws/app".into());
    f.branches[2].branch.worktree = Some("/real/ws/app-linked".into());
    f.branches[3].branch.worktree = Some("/real/ws/app-linked-2".into());
    f.worktrees = vec![
        linked("/ws/app-linked", on("linked")),
        linked("/ws/app-linked-2", on("linked-ahead")),
    ];
    let e = owned(Mode::Follow("main"));

    let clean = verdicts(&e, &f);
    assert_eq!(
        clean,
        named(&[
            ("main", act(ff(2))),
            ("other", act(ff(3))),
            // a clean linked worktree holds nothing
            ("linked", act(ff(1))),
            ("linked-ahead", act(SyncAction::Push { commits: 1 })),
        ])
    );

    f.status.uncommitted.unstaged = 1;
    let dirty = verdicts(&e, &f);
    assert_eq!(
        dirty[..3],
        named(&[
            ("main", held(ff(2), HeldBy::DirtyCheckout)),
            // not checked out: moves in place
            ("other", act(ff(3))),
            // its own checkout is clean
            ("linked", act(ff(1))),
        ])
    );

    // a dirty linked worktree holds its own branch's fast-forward, and a
    // push on a branch checked out in one still acts
    f.status.uncommitted.unstaged = 0;
    f.worktrees[0].uncommitted.untracked = 1;
    f.worktrees[1].uncommitted.staged = 1;
    assert_eq!(
        verdicts(&e, &f)[2..],
        named(&[
            ("linked", held(ff(1), HeldBy::DirtyCheckout)),
            ("linked-ahead", act(SyncAction::Push { commits: 1 })),
        ])
    );

    // ahead, checked out in the dirty primary: the push still acts
    f.status.uncommitted.unstaged = 1;
    f.branches[0].branch.track = Track::Ahead(2);
    f.branches[0].unique_commits = 2;
    assert_eq!(verdicts(&e, &f)[0].1, act(SyncAction::Push { commits: 2 }));

    // an entry-level reason outranks the checkout, and holds the push too
    f.in_progress = Some(InProgressOp::Merge);
    assert_eq!(
        verdicts(&e, &f)[0].1,
        held(SyncAction::Push { commits: 2 }, HeldBy::Entry)
    );
}

fn verdicts_with(entry: &Entry, f: &RepoFacts, sessions: &EntrySessions) -> Vec<Verdict> {
    classify(entry, f, sessions, Refresh::Unasked)
        .branches
        .into_iter()
        .map(|b| b.verdict)
        .collect()
}

/// Sessions in the checkouts at `paths`, one each.
fn busy_at(paths: &[&str]) -> EntrySessions {
    let mut sessions = EntrySessions::idle();
    for (pid, path) in (1..).zip(paths) {
        sessions.busy.insert(
            (*path).to_owned(),
            vec![Session::at(
                pid,
                0,
                (*path).to_owned(),
                SessionSource::SessionFile,
            )],
        );
    }
    sessions
}

#[test]
fn a_busy_checkout_holds_every_action_on_its_branches() {
    let ff = |commits| SyncAction::FastForward { commits };
    let push = |commits| SyncAction::Push { commits };
    let held = |action, by| Verdict::Held { action, by };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)).unique(1),
            b("linked", O, true, Track::Behind(2)),
            b("other", O, true, Track::Ahead(3)).unique(3),
        ],
    );
    f.worktrees = vec![linked("/ws/app-linked", on("linked"))];
    let e = owned(Mode::Follow("main"));
    assert_eq!(
        verdicts_with(&e, &f, &EntrySessions::idle()),
        [act(push(1)), act(ff(2)), act(push(3))]
    );

    // pushes included; a branch checked out nowhere acts
    let both = busy_at(&["/ws/app", "/ws/app-linked"]);
    assert_eq!(
        verdicts_with(&e, &f, &both),
        [
            held(push(1), HeldBy::Busy),
            held(ff(2), HeldBy::Busy),
            act(push(3)),
        ]
    );
    // a busy checkout outranks its dirt
    f.worktrees[0].uncommitted.unstaged = 1;
    assert_eq!(verdicts_with(&e, &f, &both)[1], held(ff(2), HeldBy::Busy));
    // a session only in the linked worktree leaves the primary's branch
    assert_eq!(
        verdicts_with(&e, &f, &busy_at(&["/ws/app-linked"]))[0],
        act(push(1))
    );
    // an entry-level reason outranks it
    f.in_progress = Some(InProgressOp::Merge);
    assert_eq!(
        verdicts_with(&e, &f, &both)[0],
        held(push(1), HeldBy::Entry)
    );
    f.in_progress = None;

    // an unprobed worktree whose HEAD is unknown may be on any branch:
    // a session there holds them all
    f.worktrees.clear();
    f.unprobed = vec![UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..unprobed("/ws/app-lost", None, UnprobedWhy::Missing)
    }];
    let c = classify(&e, &f, &busy_at(&["/ws/app-lost"]), Refresh::Unasked);
    assert_eq!(
        c.branches
            .iter()
            .map(|b| b.verdict.clone())
            .collect::<Vec<_>>(),
        [
            held(push(1), HeldBy::Busy),
            held(ff(2), HeldBy::Busy),
            held(push(3), HeldBy::Busy),
        ]
    );
    assert_eq!(c.unprobed[0].busy.len(), 1);
}

#[test]
fn unavailable_detection_holds_every_action() {
    let push = |commits| SyncAction::Push { commits };
    let held = |action, by| Verdict::Held { action, by };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(2)),
            b("other", O, true, Track::Ahead(3)).unique(3),
            b("old", O, true, Track::Gone),
        ],
    );
    f.status.uncommitted.untracked = 1;
    let mut wt = linked("/ws/app-old", on("old"));
    wt.submodules = Some(false);
    f.worktrees = vec![wt];
    let e = owned(Mode::Follow("main"));
    let gone = |removable: Option<&str>| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree: removable.map(str::to_owned),
    };
    assert_eq!(
        verdicts_with(&e, &f, &EntrySessions::idle())[1..],
        [act(push(3)), gone(Some("/ws/app-old"))]
    );
    assert_eq!(
        verdicts_with(&e, &f, &EntrySessions::unavailable()),
        [
            // the checkout's own reason names the hold
            held(
                SyncAction::FastForward { commits: 2 },
                HeldBy::DirtyCheckout
            ),
            // checked out nowhere, held all the same
            held(push(3), HeldBy::BusyUnknown),
            // a session there can't be ruled out
            gone(None),
        ]
    );
    // a busy worktree isn't removable either
    assert_eq!(
        verdicts_with(&e, &f, &busy_at(&["/ws/app-old"]))[2],
        gone(None)
    );
}

#[test]
fn an_unresolvable_checkout_holds_the_branches_checked_out_there() {
    use crate::busy::UnresolvedCheckout;
    let ff = |commits| SyncAction::FastForward { commits };
    let push = |commits| SyncAction::Push { commits };
    let held = |action, by| Verdict::Held { action, by };
    let unresolved = |paths: &[&str]| {
        let mut sessions = EntrySessions::idle();
        for path in paths {
            sessions.unresolved.insert(
                (*path).to_owned(),
                UnresolvedCheckout {
                    path: (*path).to_owned(),
                    error: "Permission denied (os error 13)".into(),
                },
            );
        }
        sessions
    };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)).unique(1),
            b("locked", O, true, Track::Behind(2)),
            b("linked", O, true, Track::Ahead(3)).unique(3),
            b("other", O, true, Track::Ahead(4)).unique(4),
            b("old", O, true, Track::Gone),
        ],
    );
    f.worktrees = vec![
        linked("/ws/sealed/app-linked", on("linked")),
        linked("/ws/sealed/app-old", on("old")),
        linked(
            "/ws/sealed/app-detached",
            Head::Detached {
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
        ),
    ];
    f.unprobed = vec![unprobed(
        "/ws/sealed/app-locked",
        Some("locked"),
        UnprobedWhy::Failed {
            error: "checking /ws/sealed/app-locked/.git: Permission denied (os error 13)".into(),
        },
    )];
    let e = owned(Mode::Follow("main"));
    let gone = |removable: Option<&str>| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree: removable.map(str::to_owned),
    };
    assert_eq!(
        verdicts_with(&e, &f, &EntrySessions::idle()),
        [
            act(push(1)),
            // unprobed: its fast-forward held, not a push
            held(ff(2), HeldBy::UnprobedWorktree),
            act(push(3)),
            act(push(4)),
            gone(Some("/ws/sealed/app-old")),
        ]
    );

    // every worktree unresolvable, in the order the checkouts are
    // probed: each holds what's checked out there, pushes included, and
    // the most specific reason names the hold; the branches checked out
    // nowhere, and the detached worktree's HEAD, hold nothing
    let all = unresolved(&[
        "/ws/sealed/app-locked",
        "/ws/sealed/app-detached",
        "/ws/sealed/app-old",
        "/ws/sealed/app-linked",
    ]);
    let c = classify(&e, &f, &all, Refresh::Unasked);
    let reason = |checkout: &str| NeedsHuman::CheckoutUnresolvable {
        checkout: checkout.into(),
        path: checkout.into(),
        error: "Permission denied (os error 13)".into(),
    };
    assert_eq!(
        c.needs_human,
        [
            reason("/ws/sealed/app-linked"),
            reason("/ws/sealed/app-old"),
            reason("/ws/sealed/app-detached"),
            reason("/ws/sealed/app-locked"),
        ]
    );
    assert!(!c.needs_human.iter().any(NeedsHuman::holds_entry));
    assert_eq!(
        c.branches
            .into_iter()
            .map(|b| b.verdict)
            .collect::<Vec<_>>(),
        [
            act(push(1)),
            held(ff(2), HeldBy::UnprobedWorktree),
            held(push(3), HeldBy::BusyUnknown),
            act(push(4)),
            // a session there can't be ruled out
            gone(None),
        ]
    );
    // the primary too, alone
    assert_eq!(
        verdicts_with(&e, &f, &unresolved(&["/ws/app"])),
        [
            held(push(1), HeldBy::BusyUnknown),
            held(ff(2), HeldBy::UnprobedWorktree),
            act(push(3)),
            act(push(4)),
            gone(Some("/ws/sealed/app-old")),
        ]
    );
    // an unprobed worktree whose HEAD is unknown may be on any branch
    f.unprobed[0].head = UnprobedHead::Unknown;
    assert_eq!(
        verdicts_with(&e, &f, &unresolved(&["/ws/sealed/app-locked"])),
        [
            held(push(1), HeldBy::BusyUnknown),
            held(ff(2), HeldBy::UnprobedWorktree),
            held(push(3), HeldBy::BusyUnknown),
            held(push(4), HeldBy::BusyUnknown),
            // it may be on `old` too: deleting `old` could strand it
            Verdict::Quiet,
        ]
    );
}

#[test]
fn a_worktree_that_was_not_probed_holds_all_but_a_push() {
    // git says each is checked out, but no probed checkout has them on
    // HEAD: the worktree's dir is gone, or its probe failed
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("gone-wt", O, true, Track::Behind(1)),
            b("gone-wt-ahead", O, true, Track::Ahead(1)).unique(1),
            b("failed-wt-ahead", O, true, Track::Ahead(1)).unique(1),
        ],
    );
    f.branches[1].branch.worktree = Some("/ws/app-gone".into());
    f.branches[2].branch.worktree = Some("/ws/app-gone-2".into());
    f.branches[3].branch.worktree = Some("/ws/app-failed".into());
    f.unprobed = vec![
        unprobed("/ws/app-gone", Some("gone-wt"), UnprobedWhy::Prunable),
        unprobed(
            "/ws/app-gone-2",
            Some("gone-wt-ahead"),
            UnprobedWhy::Missing,
        ),
        unprobed(
            "/ws/app-failed",
            Some("failed-wt-ahead"),
            UnprobedWhy::Failed {
                error: "boom".into(),
            },
        ),
    ];
    // a probed linked worktree on another branch doesn't count
    f.worktrees = vec![linked("/ws/app-other", on("main-2"))];
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[
            ("main", Verdict::Quiet),
            (
                "gone-wt",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: HeldBy::UnprobedWorktree,
                }
            ),
            // a push only moves refs: a session in the worktree's files,
            // wherever they are, would have made it busy
            ("gone-wt-ahead", act(SyncAction::Push { commits: 1 })),
            ("failed-wt-ahead", act(SyncAction::Push { commits: 1 })),
        ])
    );
}

#[test]
fn an_unprobed_worktree_holds_its_push_only_when_busy() {
    // gone, moved by hand, on media mounted elsewhere, or named by no
    // path: wherever its files are, a session in them is attributed to
    // it by the `.git` it finds — so with none there, its push acts
    let push = |commits| SyncAction::Push { commits };
    let held = |by| Verdict::Held {
        action: push(1),
        by,
    };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)).unique(1),
            b("moved", O, true, Track::Ahead(1)).unique(1),
            b("usb", O, true, Track::Ahead(1)).unique(1),
            b("nameless", O, true, Track::Ahead(1)).unique(1),
            b("busy", O, true, Track::Ahead(1)).unique(1),
            b("old", O, true, Track::Gone),
        ],
    );
    f.unprobed = vec![
        unprobed("/ws/app-moved", Some("moved"), UnprobedWhy::Prunable),
        unprobed("/media/usb/app", Some("usb"), UnprobedWhy::Missing),
        // its `gitdir` unreadable: the path is its own git dir
        unprobed(
            "/ws/app/.git/worktrees/n",
            Some("nameless"),
            UnprobedWhy::Failed { error: "x".into() },
        ),
        // a session in its files: attributed, wherever they are
        unprobed("/ws/app-busy", Some("busy"), UnprobedWhy::Prunable),
        unprobed("/ws/app-old", Some("old"), UnprobedWhy::Prunable),
    ];
    let e = owned(Mode::Follow("main"));
    let classified = classify(&e, &f, &busy_at(&["/ws/app-busy"]), Refresh::Unasked);
    let verdicts: Vec<(String, Verdict)> = classified
        .branches
        .into_iter()
        .map(|b| (b.name, b.verdict))
        .collect();
    assert_eq!(
        verdicts,
        named(&[
            ("main", act(push(1))),
            ("moved", act(push(1))),
            ("usb", act(push(1))),
            ("nameless", act(push(1))),
            ("busy", held(HeldBy::Busy)),
            // cleanup isn't an action, and a gone worktree is never the
            // one to remove: its own cleanup is its prune, kept
            (
                "old",
                Verdict::Cleanup {
                    reason: CleanupReason::UpstreamGone,
                    removable_worktree: None,
                }
            ),
        ])
    );
    let prunes: Vec<Option<Prune>> = classified.unprobed.into_iter().map(|u| u.prune).collect();
    assert_eq!(
        prunes,
        [
            Some(Prune::Safe),
            None,
            None,
            Some(Prune::Safe),
            Some(Prune::Safe),
        ]
    );

    // its HEAD unknown too: a session attributed to it holds every
    // branch, the primary's included
    f.unprobed = vec![UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..unprobed(
            "/ws/app/.git/worktrees/n",
            None,
            UnprobedWhy::Failed { error: "x".into() },
        )
    }];
    assert_eq!(
        verdicts_with(&e, &f, &EntrySessions::idle())[..5],
        [
            act(push(1)),
            act(push(1)),
            act(push(1)),
            act(push(1)),
            act(push(1)),
        ]
    );
    assert_eq!(
        verdicts_with(&e, &f, &busy_at(&["/ws/app/.git/worktrees/n"]))[..5],
        [
            held(HeldBy::Busy),
            held(HeldBy::Busy),
            held(HeldBy::Busy),
            held(HeldBy::Busy),
            held(HeldBy::Busy),
        ]
    );
}

#[test]
fn unprobed_worktrees_hold_by_name_and_git_is_trusted_for_the_rest() {
    let held = Verdict::Held {
        action: SyncAction::FastForward { commits: 1 },
        by: HeldBy::UnprobedWorktree,
    };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("listed", O, true, Track::Behind(1)),
            b("unlisted", O, true, Track::Behind(1)),
            b("unlisted-ahead", O, true, Track::Ahead(1)).unique(1),
        ],
    );
    // named only by the worktree list: `%(worktreepath)` is empty
    f.unprobed = vec![unprobed(
        "/ws/app-failed",
        Some("listed"),
        UnprobedWhy::Failed {
            error: "boom".into(),
        },
    )];
    // named only by `%(worktreepath)`: fail closed — and a session
    // there is scoped to no checkout, so it may be busy
    f.branches[2].branch.worktree = Some("/ws/app-somewhere".into());
    f.branches[3].branch.worktree = Some("/ws/app-elsewhere".into());
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f)[1..],
        named(&[
            ("listed", held.clone()),
            ("unlisted", held),
            (
                "unlisted-ahead",
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::BusyUnknown,
                }
            ),
        ])
    );

    // a bare repo's main worktree, which git names for its HEAD's
    // branch, has no files: nothing there to hold
    f.bare_main = Some("/ws/app-elsewhere".into());
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f)[3],
        (
            "unlisted-ahead".to_owned(),
            act(SyncAction::Push { commits: 1 })
        )
    );
}

#[test]
fn a_worktree_whose_head_is_unknown_holds_every_fast_forward() {
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(1)),
            b("ahead", O, true, Track::Ahead(1)).unique(1),
            b("gone", O, true, Track::Gone),
            b("merged", None, false, Track::Even),
        ],
    );
    f.branches[2].branch.worktree = Some("/ws/app-gone".into());
    f.worktrees = vec![linked("/ws/app-gone", on("gone"))];
    f.unprobed = vec![UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..unprobed(
            "/ws/app/.git/worktrees/x",
            None,
            UnprobedWhy::Failed {
                error: "not listed by git".into(),
            },
        )
    }];
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[
            (
                "main",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: HeldBy::UnprobedWorktree,
                }
            ),
            // a push only moves refs
            ("ahead", act(SyncAction::Push { commits: 1 })),
            // it might be checked out there too: deleting it could
            // strand that worktree, so it's no cleanup
            ("gone", Verdict::Quiet),
            // it might be checked out there: a fresh branch, not merged
            ("merged", Verdict::Quiet),
        ])
    );
}

#[test]
fn an_unreadable_git_dir_holds_the_entry_pushes_too() {
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(1)),
            b("ahead", O, true, Track::Ahead(1)).unique(1),
        ],
    );
    f.unreadable = vec!["/ws/app/.git/worktrees".into()];
    let c = classify(
        &owned(Mode::Follow("main")),
        &f,
        &EntrySessions::idle(),
        Refresh::Unasked,
    );
    assert_eq!(
        c.needs_human,
        [NeedsHuman::WorktreeUnreadable {
            path: "/ws/app/.git/worktrees".into()
        }]
    );
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[
            (
                "main",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: HeldBy::Entry,
                }
            ),
            (
                "ahead",
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::Entry,
                }
            ),
        ])
    );
}

#[test]
fn what_a_prune_would_lose() {
    let f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("feat", O, true, Track::Even),
        ],
    );
    let gone = unprobed("/ws/app-gone", Some("feat"), UnprobedWhy::Prunable);
    let loses = |losses| Some(Prune::Loses { losses });
    // on a branch that exists, at rest: nothing
    assert_eq!(prune(&gone, &f), Some(Prune::Safe));
    // a merge keeps HEAD on its branch: the operation alone is lost
    let merging = UnprobedWorktree {
        in_progress: Some(InProgressOp::Merge),
        ..gone.clone()
    };
    assert_eq!(
        prune(&merging, &f),
        loses(vec![PruneLoss::Operation {
            op: InProgressOp::Merge
        }])
    );
    let detached = unprobed("/ws/app-gone", None, UnprobedWhy::Prunable);
    assert_eq!(prune(&detached, &f), loses(vec![PruneLoss::DetachedHead]));
    let unknown = UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..gone.clone()
    };
    assert_eq!(prune(&unknown, &f), loses(vec![PruneLoss::UnknownHead]));
    // its branch deleted: its HEAD may be the only ref to its commit
    let orphaned = unprobed("/ws/app-gone", Some("deleted"), UnprobedWhy::Prunable);
    assert_eq!(
        prune(&orphaned, &f),
        loses(vec![PruneLoss::MissingBranch {
            name: "deleted".into()
        }])
    );
    // every loss, listed
    let both = UnprobedWorktree {
        in_progress: Some(InProgressOp::Rebase),
        ..detached
    };
    assert_eq!(
        prune(&both, &f),
        loses(vec![
            PruneLoss::Operation {
                op: InProgressOp::Rebase
            },
            PruneLoss::DetachedHead
        ])
    );
    // a git dir that can't be matched can't be read
    let unmatched = UnprobedWorktree {
        git_dir: None,
        ..gone.clone()
    };
    assert_eq!(
        prune(&unmatched, &f),
        loses(vec![PruneLoss::UnmatchedGitDir])
    );
    // what its git dir alone holds
    let holding = |submodules, worktree_refs, staged| UnprobedWorktree {
        holds: Some(GitDirHolds {
            submodules,
            worktree_refs,
            staged,
        }),
        ..gone.clone()
    };
    assert_eq!(
        prune(&holding(false, false, Some(false)), &f),
        Some(Prune::Safe)
    );
    assert_eq!(
        prune(&holding(true, true, Some(true)), &f),
        loses(vec![
            PruneLoss::Submodules,
            PruneLoss::WorktreeRefs,
            PruneLoss::StagedChanges
        ])
    );
    // an index that couldn't be compared counts as staged...
    assert_eq!(
        prune(&holding(false, false, None), &f),
        loses(vec![PruneLoss::StagedChanges])
    );
    // ...unless its HEAD is already lost, which says as much
    let lost_head = UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..holding(false, false, None)
    };
    assert_eq!(prune(&lost_head, &f), loses(vec![PruneLoss::UnknownHead]));
    let lost_branch = UnprobedWorktree {
        head: UnprobedHead::Branch {
            name: "deleted".into(),
        },
        ..holding(false, false, None)
    };
    assert_eq!(
        prune(&lost_branch, &f),
        loses(vec![PruneLoss::MissingBranch {
            name: "deleted".into()
        }])
    );
    // a relative `gitdir` anywhere in the repo: no path is certain, so
    // nothing is safe
    let relative = RepoFacts {
        relative_gitdir: Some(PathBuf::from("/ws/app/.git/worktrees/k")),
        ..f.clone()
    };
    assert_eq!(
        prune(&gone, &relative),
        loses(vec![PruneLoss::RelativeGitdir {
            git_dir: "/ws/app/.git/worktrees/k".into()
        }])
    );
    // not gone: no prune at all
    for why in [
        UnprobedWhy::Missing,
        UnprobedWhy::Failed { error: "x".into() },
    ] {
        let u = UnprobedWorktree {
            why,
            ..gone.clone()
        };
        assert_eq!(prune(&u, &f), None);
    }
}

#[test]
fn every_checkout_on_a_branch_counts() {
    // git allows one branch on HEAD in several checkouts
    // (`worktree add -f`); any dirty one holds it, and clean ones hold
    // its ff too: moving it in one would strand the others' HEAD
    let ff1 = Verdict::Held {
        action: SyncAction::FastForward { commits: 1 },
        by: HeldBy::SeveralCheckouts,
    };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(1)),
            b("twice", O, true, Track::Gone),
        ],
    );
    f.branches[0].branch.worktree = Some("/ws/app".into());
    f.branches[1].branch.worktree = Some("/ws/app-twice-1".into());
    f.worktrees = vec![
        linked("/ws/app-main", on("main")),
        linked("/ws/app-twice-1", on("twice")),
        linked("/ws/app-twice-2", on("twice")),
    ];
    let e = owned(Mode::Follow("main"));
    let v = verdicts(&e, &f);
    assert_eq!(v[0].1, ff1);
    // two clean worktrees on it: neither is the branch's to remove
    assert_eq!(
        v[1].1,
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: None,
        }
    );

    // a clean primary doesn't hide a dirty worktree on the same branch
    f.worktrees[0].uncommitted.unstaged = 1;
    assert_eq!(
        verdicts(&e, &f)[0].1,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::DirtyCheckout,
        }
    );
    // nor does a clean worktree hide an unprobed one
    f.worktrees[0].uncommitted.unstaged = 0;
    f.unprobed = vec![unprobed(
        "/ws/app-main-2",
        Some("main"),
        UnprobedWhy::Missing,
    )];
    assert_eq!(
        verdicts(&e, &f)[0].1,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::UnprobedWorktree,
        }
    );
    // dirty outranks unknown
    f.status.uncommitted.untracked = 1;
    assert_eq!(
        verdicts(&e, &f)[0].1,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::DirtyCheckout,
        }
    );
    // on one checkout, clean: the ff acts
    f.status.uncommitted.untracked = 0;
    f.unprobed.clear();
    f.worktrees.remove(0);
    assert_eq!(
        verdicts(&e, &f)[0].1,
        Verdict::Act {
            action: SyncAction::FastForward { commits: 1 },
        }
    );
}

#[test]
fn a_failed_fetch_holds_every_action() {
    let ff = |commits| SyncAction::FastForward { commits };
    let held = |action, by| Verdict::Held { action, by };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(2)),
            b("ahead", O, true, Track::Ahead(1)),
            b("idle", O, true, Track::Behind(1)),
        ],
    );
    f.fetch_failed = true;
    let e = owned(Mode::Follow("main"));
    assert_eq!(
        verdicts(&e, &f),
        named(&[
            ("main", held(ff(2), HeldBy::FetchFailed)),
            (
                "ahead",
                held(SyncAction::Push { commits: 1 }, HeldBy::FetchFailed)
            ),
            ("idle", held(ff(1), HeldBy::FetchFailed)),
        ])
    );
    // an entry-level reason outranks it
    f.in_progress = Some(InProgressOp::Merge);
    assert_eq!(verdicts(&e, &f)[0].1, held(ff(2), HeldBy::Entry));
}

#[test]
fn a_worktree_is_removable_only_when_git_would_remove_it() {
    let cleanup = |removable_worktree: Option<&str>| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree: removable_worktree.map(str::to_owned),
    };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("plain", O, true, Track::Gone),
            b("locked", O, true, Track::Gone),
            b("picking", O, true, Track::Gone),
            b("in-main", O, true, Track::Gone),
            b("with-submodules", O, true, Track::Gone),
            b("unchecked", O, true, Track::Gone),
        ],
    );
    let mut locked = linked("/ws/app-locked", on("locked"));
    locked.locked = true;
    let mut picking = linked("/ws/app-picking", on("picking"));
    picking.in_progress = Some(InProgressOp::CherryPick);
    // the main worktree, when the registry's dir is a linked one
    let mut main_wt = linked("/ws/app-main", on("in-main"));
    main_wt.linked = false;
    let mut submodules = linked("/ws/app-subs", on("with-submodules"));
    submodules.submodules = Some(true);
    // clean, unlocked, linked, but its submodules weren't checked
    let mut unchecked = linked("/ws/app-unchecked", on("unchecked"));
    unchecked.submodules = None;
    f.worktrees = vec![
        linked("/ws/app-plain", on("plain")),
        locked,
        picking,
        main_wt,
        submodules,
        unchecked,
    ];
    for i in 1..7 {
        f.branches[i].branch.worktree = Some(f.worktrees[i - 1].path.clone());
    }
    let v = verdicts(&owned(Mode::Follow("main")), &f);
    assert_eq!(
        v[1..],
        named(&[
            ("plain", cleanup(Some("/ws/app-plain"))),
            // `git worktree remove` refuses a locked worktree
            ("locked", cleanup(None)),
            ("picking", cleanup(None)),
            // `git worktree remove` refuses the main worktree, and one
            // with initialized submodules
            ("in-main", cleanup(None)),
            ("with-submodules", cleanup(None)),
            // unknown is not "none"
            ("unchecked", cleanup(None)),
        ])
    );
}

#[test]
fn a_gone_branch_in_a_clean_linked_worktree_is_removable() {
    let cleanup = |removable_worktree: Option<&str>| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree: removable_worktree.map(str::to_owned),
    };
    let mut f = facts(
        on("in-primary"),
        &[
            b("main", O, true, Track::Even),
            b("in-clean", O, true, Track::Gone).unique(1),
            b("in-dirty", O, true, Track::Gone),
            b("in-primary", O, true, Track::Gone),
            b("in-none", O, true, Track::Gone),
            b("in-unprobed", O, true, Track::Gone),
            b("merged-in-wt", None, false, Track::Even),
        ],
    );
    for (i, path) in [
        (1, "/ws/app-clean"),
        (2, "/ws/app-dirty"),
        (3, "/ws/app"),
        (5, "/ws/app-unprobed"),
        (6, "/ws/app-merged"),
    ] {
        f.branches[i].branch.worktree = Some(path.into());
    }
    let mut dirty = linked("/ws/app-dirty", on("in-dirty"));
    dirty.uncommitted.unstaged = 1;
    f.worktrees = vec![
        linked("/ws/app-clean", on("in-clean")),
        dirty,
        linked("/ws/app-merged", on("merged-in-wt")),
    ];
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[
            ("main", Verdict::Quiet),
            ("in-clean", cleanup(Some("/ws/app-clean"))),
            // not removable: its dirt shows as uncommitted instead
            ("in-dirty", cleanup(None)),
            // the primary checkout is never removable
            ("in-primary", cleanup(None)),
            ("in-none", cleanup(None)),
            ("in-unprobed", cleanup(None)),
            // nothing unique and checked out reads as a fresh branch
            ("merged-in-wt", Verdict::Quiet),
        ])
    );
}

#[test]
fn a_push_url_other_than_the_registrys_holds_pushes_only() {
    let push = SyncAction::Push { commits: 1 };
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)),
            b("feat", O, true, Track::Behind(1)),
        ],
    );
    let e = owned(Mode::Follow("main"));
    let reasons = |f: &RepoFacts| {
        classify(&e, f, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .into_iter()
            .filter(|r| matches!(r, NeedsHuman::PushUrlMismatch { .. }))
            .collect::<Vec<_>>()
    };
    // the registry's repo over SSH, however spelled
    for url in [
        "git@github.com:me/app",
        "ssh://git@github.com/me/app.git",
        "git+ssh://git@github.com/Me/App/",
    ] {
        f.push_urls = Some(vec![url.into()]);
        assert_eq!(reasons(&f), [], "{url}");
        assert_eq!(verdicts(&e, &f)[0].1, act(push), "{url}");
    }
    // another repo, another transport, several URLs, or none
    for urls in [
        &["git@github.com:me/other"][..],
        &["git@evil.example.com:me/app"],
        &["https://github.com/me/app"],
        &["https://tok@github.com/me/app"],
        &["file:///srv/me/app.git"],
        &["/srv/me/app"],
        &["git@github.com:me/app", "git@github.com:me/app"],
        &[],
    ] {
        f.push_urls = Some(urls.iter().map(|u| (*u).to_owned()).collect());
        assert_eq!(
            reasons(&f),
            [NeedsHuman::PushUrlMismatch {
                push_urls: urls.iter().map(|u| u.replace("tok@", "***@")).collect(),
                expected: "git@github.com:me/app".into(),
            }],
            "{urls:?}"
        );
        assert_eq!(
            verdicts(&e, &f),
            named(&[
                (
                    "main",
                    Verdict::Held {
                        action: push,
                        by: HeldBy::PushUrl
                    }
                ),
                ("feat", act(SyncAction::FastForward { commits: 1 })),
            ]),
            "{urls:?}"
        );
    }
    // a lookalike, however much of the registry's URL it spells
    for lookalike in LOOKALIKE_URLS {
        assert!(
            !push_urls_match(&[lookalike.to_owned()], &e.url),
            "{lookalike}"
        );
        f.push_urls = Some(vec![lookalike.to_owned()]);
        assert_eq!(
            verdicts(&e, &f)[0].1,
            Verdict::Held {
                action: push,
                by: HeldBy::PushUrl
            },
            "{lookalike}"
        );
    }
    // origin drift says it first, and holds the entry
    f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/other")];
    assert_eq!(reasons(&f), []);
    // not read: nothing to say
    f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/app")];
    f.push_urls = None;
    assert_eq!(reasons(&f), []);
}

#[test]
fn a_push_names_only_a_branch_on_origin() {
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)),
            b("tip", O, true, Track::Ahead(2)),
            b("tag", O, true, Track::Ahead(1)),
            b("lag", O, true, Track::Behind(1)),
        ],
    );
    // `origin/HEAD` as an upstream, and a ref outside `refs/heads/`
    f.branches[1].branch.merge_ref = Some("refs/heads/HEAD".into());
    f.branches[2].branch.merge_ref = Some("refs/tags/v1".into());
    // behind: a fast-forward names no ref on origin
    f.branches[3].branch.merge_ref = Some("refs/heads/HEAD".into());
    assert_eq!(push_target(&f.branches[0].branch), Some("refs/heads/main"));
    assert_eq!(push_target(&f.branches[1].branch), None);
    assert_eq!(push_target(&f.branches[2].branch), None);
    // a ref name git itself refuses: never named on the remote
    let mut odd = f.branches[0].branch.clone();
    for merge in [
        "refs/heads/a..b",
        "refs/heads/x.lock",
        "refs/heads/a b",
        "refs/heads/a:b",
        "refs/heads/.hidden",
        "refs/heads/a//b",
        "refs/heads/a/",
        "refs/heads/a@{1}",
        "refs/heads/end.",
    ] {
        odd.merge_ref = Some(merge.into());
        assert_eq!(push_target(&odd), None, "{merge}");
    }
    odd.merge_ref = Some("refs/heads/feat/x-1.2".into());
    assert_eq!(push_target(&odd), Some("refs/heads/feat/x-1.2"));
    let e = owned(Mode::Follow("main"));
    assert_eq!(
        verdicts(&e, &f),
        named(&[
            ("main", act(SyncAction::Push { commits: 1 })),
            ("tip", needs(BranchNeedsHuman::UpstreamNotABranch)),
            ("tag", needs(BranchNeedsHuman::UpstreamNotABranch)),
            ("lag", act(SyncAction::FastForward { commits: 1 })),
        ])
    );
    // archived says it first
    let archived = Entry {
        archived: true,
        ..e
    };
    assert_eq!(
        verdicts(&archived, &f)[1].1,
        needs(BranchNeedsHuman::ArchivedAhead)
    );
}

#[test]
fn a_refresh_is_asked_of_third_party_references_and_refused_by_pins() {
    use RefreshVerdict::{Act, Held};
    let refused = Some(Held { by: HeldBy::Pinned });
    // (entry, unasked, named, --references)
    let table = [
        (owned(Mode::Follow("main")), None, None, None),
        (owned(Mode::PinnedOn("fork")), None, refused, None),
        (third_party(Mode::Head), None, Some(Act), Some(Act)),
        (third_party(Mode::Pinned), None, refused, None),
    ];
    for (e, unasked, named, references) in table {
        // its origin the registry's repo, the verdict is the intent
        let config = ConfigFacts {
            origin_urls: vec![OriginUrl::repo(&e.remote_url())],
            origin_keys: OriginKeys::InRepo,
            origin_fetch_url: Some(e.remote_url()),
            ..ConfigFacts::default()
        };
        for (refresh, want) in [
            (Refresh::Unasked, unasked),
            (Refresh::Named, named),
            (Refresh::References, references),
        ] {
            assert_eq!(refresh_intent(&e, refresh), want, "{e:?} {refresh:?}");
            assert_eq!(
                refresh_verdict(&e, refresh, &config),
                want,
                "{e:?} {refresh:?}"
            );
        }
    }
}

/// A refresh of a repo whose origin isn't the registry's repo is held
/// for its origin drift, never fetched: another URL (an SSH fork, an
/// HTTPS fork), an origin with no URL, or none. A pin named stays
/// refused by its pin; an owned entry is never refreshed.
#[test]
fn a_refresh_with_origin_drift_is_held() {
    use RefreshVerdict::{Act, Held};
    let drifted = Some(Held { by: HeldBy::Entry });
    let with_origin = |url: Option<&str>| ConfigFacts {
        origin_urls: url.map(OriginUrl::repo).into_iter().collect(),
        origin_keys: if url.is_some() {
            OriginKeys::InRepo
        } else {
            OriginKeys::None
        },
        origin_fetch_url: url.map(str::to_owned),
        ..ConfigFacts::default()
    };
    let lib = third_party(Mode::Head);
    for refresh in [Refresh::Named, Refresh::References] {
        for (origin, want) in [
            (Some("https://github.com/them/lib"), Some(Act)),
            (Some("https://github.com/Them/lib.git/"), Some(Act)),
            // the same repo over SSH, spelled otherwise: no drift, but
            // not the HTTPS a reference is fetched over
            (
                Some("git@github.com:Them/lib.git"),
                Some(Held {
                    by: HeldBy::OriginNotHttps,
                }),
            ),
            (Some("git@github.com:me/lib"), drifted),
            (Some("https://github.com/me/lib"), drifted),
            (Some("https://github.com/them/lib2"), drifted),
            (None, drifted),
        ] {
            assert_eq!(
                refresh_verdict(&lib, refresh, &with_origin(origin)),
                want,
                "{origin:?} {refresh:?}"
            );
        }
        let mut no_url = with_origin(None);
        no_url.origin_keys = OriginKeys::InRepo;
        assert_eq!(refresh_verdict(&lib, refresh, &no_url), drifted);
        assert_eq!(
            refresh_verdict(&lib, Refresh::Unasked, &with_origin(None)),
            None
        );
    }
    let fork = with_origin(Some("git@github.com:me/lib"));
    assert_eq!(
        refresh_verdict(&third_party(Mode::Pinned), Refresh::Named, &fork),
        Some(Held { by: HeldBy::Pinned })
    );
    assert_eq!(
        refresh_verdict(&owned(Mode::Follow("main")), Refresh::Named, &fork),
        None
    );
}

/// A refresh whose fetch wouldn't reach the registry's repo over HTTPS
/// — its origin the repo over SSH, `http://`, or `git://`, or an
/// `insteadOf` rewrite of its HTTPS origin, or a fetch URL never read —
/// is held for it, with its own reason, and tracks no branch. Its fix
/// points origin at the HTTPS URL, unless a rewrite makes the URL.
#[test]
fn a_refresh_not_over_https_is_held() {
    let held = Some(RefreshVerdict::Held {
        by: HeldBy::OriginNotHttps,
    });
    let lib = third_party(Mode::Head);
    let https = "https://github.com/them/lib";
    let cases: [(&str, Option<&str>, Option<OriginFix>); 6] = [
        (
            "git@github.com:them/lib",
            Some("git@github.com:them/lib"),
            Some(OriginFix::SetUrl),
        ),
        (
            "ssh://git@github.com/them/lib",
            Some("ssh://git@github.com/them/lib"),
            Some(OriginFix::SetUrl),
        ),
        (
            "http://github.com/them/lib",
            Some("http://github.com/them/lib"),
            Some(OriginFix::SetUrl),
        ),
        (
            "git://github.com/them/lib",
            Some("git://github.com/them/lib"),
            Some(OriginFix::SetUrl),
        ),
        // `url.git@github.com:.insteadOf=https://github.com/`
        (https, Some("git@github.com:them/lib"), None),
        // a rewrite to another host's HTTPS
        (https, Some("https://mirror.example/them/lib"), None),
    ];
    for (origin, fetch_url, fix) in cases {
        let mut f = lib_facts(on("main"), &[b("main", O, true, Track::Behind(3))]);
        f.config.origin_urls = vec![OriginUrl::repo(origin)];
        f.config.origin_fetch_url = fetch_url.map(str::to_owned);
        for refresh in [Refresh::Named, Refresh::References] {
            assert_eq!(refresh_verdict(&lib, refresh, &f.config), held, "{origin}");
            let c = classify(&lib, &f, &EntrySessions::idle(), refresh);
            assert!(c.branches.is_empty(), "{:?}", c.branches);
            assert_eq!(
                c.needs_human,
                [NeedsHuman::OriginNotHttps {
                    fetch_url: fetch_url.unwrap().to_owned(),
                    expected: https.to_owned(),
                    fix: fix.clone(),
                }],
                "{origin}"
            );
        }
        // unasked: nothing said of it
        let c = classify(&lib, &f, &EntrySessions::idle(), Refresh::Unasked);
        assert!(c.needs_human.is_empty(), "{:?}", c.needs_human);
    }
    // a fetch URL never read: held, failing closed
    let mut unread = lib_facts(on("main"), &[]);
    unread.config.origin_fetch_url = None;
    assert_eq!(refresh_verdict(&lib, Refresh::Named, &unread.config), held);
    // a pin named keeps its pin's refusal; origin drift says drift first
    let mut f = lib_facts(on("main"), &[]);
    f.config.origin_fetch_url = Some("git@github.com:them/lib".into());
    assert_eq!(
        refresh_verdict(&third_party(Mode::Pinned), Refresh::Named, &f.config),
        Some(RefreshVerdict::Held { by: HeldBy::Pinned })
    );
    f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/lib")];
    f.config.origin_fetch_url = Some("git@github.com:me/lib".into());
    assert_eq!(
        refresh_verdict(&lib, Refresh::Named, &f.config),
        Some(RefreshVerdict::Held { by: HeldBy::Entry })
    );
    let c = classify(&lib, &f, &EntrySessions::idle(), Refresh::Named);
    assert!(
        matches!(c.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
        "{:?}",
        c.needs_human
    );
}

/// An owned entry's fetch, as git resolves it, that wouldn't reach the
/// registry's repo as sync fetches it — another repo a rewrite names,
/// or a partial clone's over neither SSH nor HTTPS — holds the entry,
/// with its own reason. Its fix points origin at the SSH URL, unless a
/// rewrite makes the URL. Origin drift says drift alone, a pin is never
/// fetched, and a fetch URL never read says nothing.
#[test]
fn an_owned_fetch_that_reaches_elsewhere_holds_the_entry() {
    let app = owned(Mode::Follow("main"));
    let ssh = "git@github.com:me/app";
    let branches = [b("main", O, true, Track::Ahead(1))];
    let with = |origin: &str, fetch_url: &str, partial: bool| {
        let mut f = facts(on("main"), &branches);
        f.config.origin_urls = vec![OriginUrl::repo(origin)];
        f.config.origin_fetch_url = Some(fetch_url.to_owned());
        f.config.partial_filter = partial.then(|| "blob:none".to_owned());
        f
    };
    let cases: [(&str, &str, bool, Option<OriginFix>); 5] = [
        // a rewrite to another repo
        (ssh, "git@github.com:me/other", false, None),
        (ssh, "file:///srv/app.git", false, None),
        // a partial clone over a transport its lazy fetch may not take
        (ssh, "git://github.com/me/app", true, None),
        (
            "http://github.com/me/app",
            "http://github.com/me/app",
            true,
            Some(OriginFix::SetUrl),
        ),
        (
            "git://github.com/me/app",
            "git://github.com/me/app",
            true,
            Some(OriginFix::SetUrl),
        ),
    ];
    for (origin, fetch_url, partial, fix) in cases {
        let f = with(origin, fetch_url, partial);
        assert_eq!(
            fetch_url_mismatch(&app, &f.config),
            Some(fetch_url),
            "{fetch_url}"
        );
        let c = classify(&app, &f, &EntrySessions::idle(), Refresh::Unasked);
        assert_eq!(
            c.needs_human,
            [NeedsHuman::FetchUrlMismatch {
                fetch_url: fetch_url.to_owned(),
                expected: ssh.to_owned(),
                fix,
            }],
            "{fetch_url}"
        );
        assert!(c.needs_human[0].holds_entry());
        assert_eq!(
            c.branches[0].verdict,
            Verdict::Held {
                action: SyncAction::Push { commits: 1 },
                by: HeldBy::Entry
            },
            "{fetch_url}"
        );
    }
    // the registry's repo, however git reaches it: nothing said
    for (fetch_url, partial) in [
        (ssh, true),
        ("https://github.com/me/app", true),
        ("ssh://git@github.com/me/app.git", true),
        // a whole clone fetches over whatever names the repo
        ("git://github.com/me/app", false),
    ] {
        let f = with(ssh, fetch_url, partial);
        assert_eq!(fetch_url_mismatch(&app, &f.config), None, "{fetch_url}");
        let c = classify(&app, &f, &EntrySessions::idle(), Refresh::Unasked);
        assert!(c.needs_human.is_empty(), "{fetch_url}: {:?}", c.needs_human);
    }
    // never read: nothing said (the probe doesn't fetch it)
    let unread = facts(on("main"), &branches);
    assert_eq!(fetch_url_mismatch(&app, &unread.config), None);
    // a pin, and a reference: never this reason
    let other = with(ssh, "git@github.com:me/other", false);
    assert_eq!(
        fetch_url_mismatch(&owned(Mode::Pinned), &other.config),
        None
    );
    assert_eq!(
        fetch_url_mismatch(&third_party(Mode::Head), &other.config),
        None
    );
    // origin drift says drift alone
    let drifted = with("git@github.com:me/other", "git@github.com:me/other", false);
    let c = classify(&app, &drifted, &EntrySessions::idle(), Refresh::Unasked);
    assert!(
        matches!(c.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
        "{:?}",
        c.needs_human
    );
}

/// A drifted refresh compares no branch against origin: only local
/// work is said, as for a reference no run asks about, and the drift
/// is the entry's reason.
#[test]
fn a_drifted_refresh_tracks_no_branch() {
    let mut f = lib_facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(3)),
            b("audit", None, false, Track::Even).unique(1),
        ],
    );
    f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/lib")];
    let c = classify(
        &third_party(Mode::Head),
        &f,
        &EntrySessions::idle(),
        Refresh::Named,
    );
    let names: Vec<(&str, Relation)> = c
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.relation))
        .collect();
    assert_eq!(names, [("audit", Relation::Untracked)]);
    assert!(matches!(
        c.needs_human[..],
        [NeedsHuman::OriginMismatch { .. }]
    ));
}

/// A third-party reference's facts, its origin the registry's URL.
fn lib_facts(head: Head, branches: &[B<'_>]) -> RepoFacts {
    let mut f = facts(head, branches);
    f.config.origin_urls = vec![OriginUrl::repo("https://github.com/them/lib")];
    f.config.origin_fetch_url = Some("https://github.com/them/lib".into());
    // never read for a third-party reference
    f.push_urls = None;
    f
}

#[test]
fn a_refreshed_reference_is_compared_against_origin_and_never_pushed() {
    let mut f = lib_facts(
        on("main"),
        &[
            b("main", O, true, Track::Behind(3)),
            b("feat", O, true, Track::Behind(1)),
            // commits on no remote: local work
            b("audit", O, true, Track::Ahead(2)).unique(2),
            // ahead of origin, its commits on another remote already
            b("mirror", O, true, Track::Ahead(1)),
            b(
                "arc",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 1,
                },
            )
            .unique(1),
            b("even", O, true, Track::Even),
        ],
    );
    let lib = third_party(Mode::Head);
    let refreshed = |f: &RepoFacts, refresh| classify(&lib, f, &EntrySessions::idle(), refresh);
    for refresh in [Refresh::Named, Refresh::References] {
        let c = refreshed(&f, refresh);
        assert!(c.needs_human.is_empty(), "{:?}", c.needs_human);
        let got: Vec<_> = c
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect();
        assert_eq!(
            got,
            named(&[
                ("main", act(SyncAction::FastForward { commits: 3 })),
                ("feat", act(SyncAction::FastForward { commits: 1 })),
                ("audit", Verdict::LocalOnly),
                ("mirror", Verdict::Quiet),
                ("arc", needs(BranchNeedsHuman::Diverged)),
                ("even", Verdict::Quiet),
            ]),
            "{refresh:?}"
        );
    }
    // a dirty checkout holds the branch it's on, not the others
    f.status.uncommitted.untracked = 1;
    let c = refreshed(&f, Refresh::Named);
    assert_eq!(
        c.branches[0].verdict,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 3 },
            by: HeldBy::DirtyCheckout,
        }
    );
    assert_eq!(
        c.branches[1].verdict,
        act(SyncAction::FastForward { commits: 1 })
    );
    // unasked: local work alone, compared against nothing
    f.status.uncommitted.untracked = 0;
    let c = refreshed(&f, Refresh::Unasked);
    let got: Vec<_> = c
        .branches
        .into_iter()
        .map(|b| (b.name, b.relation, b.verdict))
        .collect();
    assert_eq!(
        got,
        [
            ("audit".to_owned(), Relation::Untracked, Verdict::LocalOnly),
            ("arc".to_owned(), Relation::Untracked, Verdict::LocalOnly),
        ]
    );
    // a pin named keeps its pin's verdicts
    let pinned = third_party(Mode::Pinned);
    assert_eq!(
        verdicts(&pinned, &f),
        classify(&pinned, &f, &EntrySessions::idle(), Refresh::Named)
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_refreshed_shallow_reference_moves_when_nothing_local_is_at_stake() {
    let mut f = lib_facts(
        on("main"),
        &[
            b(
                "main",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 1,
                },
            ),
            b("tip", O, true, Track::Ahead(1)).unique(1).on_tip(),
            b(
                "stranded",
                O,
                true,
                Track::Diverged {
                    ahead: 2,
                    behind: 1,
                },
            )
            .unique(1),
        ],
    );
    f.layout.shallow = true;
    let c = classify(
        &third_party(Mode::Head),
        &f,
        &EntrySessions::idle(),
        Refresh::Named,
    );
    let got: Vec<_> = c
        .branches
        .into_iter()
        .map(|b| (b.name, b.verdict))
        .collect();
    assert_eq!(
        got,
        named(&[
            ("main", act(SyncAction::Move)),
            // on the fetched tip, but never pushed
            ("tip", Verdict::LocalOnly),
            ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
        ])
    );
}

#[test]
fn a_missing_entry_cloned_under_another_name_is_held() {
    let stray = |dir: &str, origin: Option<&str>, kind| UnregisteredClone {
        dir: dir.into(),
        origin: origin.map(str::to_owned),
        owned: true,
        kind,
    };
    let e = owned(Mode::Follow("main"));
    let recipe = clone_recipe(&e);
    let held = |dirs: &[&str]| ClassifiedMissing {
        clone: CloneVerdict::Held {
            recipe: recipe.clone(),
            by: HeldBy::Entry,
        },
        needs_human: dirs
            .iter()
            .map(|d| NeedsHuman::ClonedUnregistered {
                dir: (*d).to_owned(),
            })
            .collect(),
    };
    // its repo by any of git's spellings, a credential redacted
    for origin in [
        "git@github.com:me/app",
        "ssh://git@github.com/me/app.git",
        "https://github.com/Me/App/",
        "https://***@github.com/me/app",
    ] {
        let found = [stray("app-old", Some(origin), UnregisteredKind::Clone)];
        assert_eq!(
            classify_missing(&e, false, false, &found),
            held(&["app-old"]),
            "{origin}"
        );
    }
    // each dir that clones it, a worktree of an unregistered clone too,
    // named before a session at the path
    let found = [
        stray("a", Some("git@github.com:me/app"), UnregisteredKind::Clone),
        stray(
            "b",
            Some("git@github.com:me/app"),
            UnregisteredKind::Worktree,
        ),
    ];
    assert_eq!(classify_missing(&e, true, false, &found), held(&["a", "b"]));
    // another repo, no origin, or the tool's own temp dir: nothing
    let unrelated = [
        stray("x", Some("git@github.com:me/apps"), UnregisteredKind::Clone),
        stray("y", Some("git@evil.com:me/app"), UnregisteredKind::Clone),
        stray("z", None, UnregisteredKind::Clone),
        stray(
            ".app.repos-clone-1-0123456789abcdef",
            Some("git@github.com:me/app"),
            UnregisteredKind::UnfinishedClone,
        ),
    ];
    assert_eq!(
        classify_missing(&e, false, false, &unrelated),
        ClassifiedMissing {
            clone: CloneVerdict::Act { recipe },
            needs_human: vec![],
        }
    );
    assert!(NeedsHuman::ClonedUnregistered { dir: "a".into() }.holds_entry());
}

/// A rename differing only in ASCII case and `-` against `_` holds the
/// clone; any other name, account, or host doesn't — nor is it origin
/// drift's or a push URL's match, which stay exact.
#[test]
fn a_missing_entry_cloned_under_its_renamed_name_is_held() {
    let e = Entry {
        url: url("https://github.com/me/vscode-extension-tsv-format"),
        ..owned(Mode::Follow("main"))
    };
    let stray = |origin: &str| UnregisteredClone {
        dir: "old".into(),
        origin: Some(origin.to_owned()),
        owned: true,
        kind: UnregisteredKind::Clone,
    };
    for origin in [
        "git@github.com:me/vscode_extension_tsv_format",
        "https://github.com/ME/VSCode_Extension-TSV_Format.git",
        "git@github.com:me/vscode-extension-tsv-format",
    ] {
        let c = classify_missing(&e, false, false, &[stray(origin)]);
        assert_eq!(
            c.needs_human,
            [NeedsHuman::ClonedUnregistered { dir: "old".into() }],
            "{origin}"
        );
        assert!(
            matches!(
                c.clone,
                CloneVerdict::Held {
                    by: HeldBy::Entry,
                    ..
                }
            ),
            "{origin}"
        );
    }
    for origin in [
        "git@github.com:me/vscode.extension.tsv.format",
        "git@github.com:me/vscode-extension-tsv-formats",
        "git@github.com:me/vscodeextensiontsvformat",
        "git@github.com:you/vscode_extension_tsv_format",
        "git@gitlab.com:me/vscode_extension_tsv_format",
        "ssh://git@github.com:22/me/vscode_extension_tsv_format",
    ] {
        let c = classify_missing(&e, false, false, &[stray(origin)]);
        assert!(c.needs_human.is_empty(), "{origin}: {:?}", c.needs_human);
        assert!(matches!(c.clone, CloneVerdict::Act { .. }), "{origin}");
    }
    let renamed = "git@github.com:me/vscode_extension_tsv_format";
    assert!(!origin_matches(renamed, &e.url));
    assert!(!push_urls_match(&[renamed.to_owned()], &e.url));
}

#[test]
fn a_missing_entry_is_cloned_by_its_recipe() {
    let act = |recipe| CloneVerdict::Act { recipe };
    // owned: over SSH, on its branch
    let owned_recipe = CloneRecipe {
        url: "git@github.com:me/app".into(),
        branch: Some("main".into()),
        shallow: false,
        sparse: None,
    };
    assert_eq!(
        classify_missing(&owned(Mode::Follow("main")), false, false, &[]).clone,
        act(owned_recipe.clone())
    );
    // third-party: over HTTPS, shallow and sparse as declared, the
    // remote's default branch without one
    let lib = Entry {
        shallow: true,
        sparse: Some("css".into()),
        ..third_party(Mode::Head)
    };
    assert_eq!(
        classify_missing(&lib, false, false, &[]).clone,
        act(CloneRecipe {
            url: "https://github.com/them/lib".into(),
            branch: None,
            shallow: true,
            sparse: Some("css".into()),
        })
    );
    // a pin and an archived repo are cloned all the same
    for e in [
        owned(Mode::PinnedOn("fork")),
        Entry {
            archived: true,
            ..owned(Mode::Follow("main"))
        },
    ] {
        assert!(
            matches!(
                classify_missing(&e, false, false, &[]).clone,
                CloneVerdict::Act { .. }
            ),
            "{e:?}"
        );
    }
    // a session at the path holds it, named before a recorded worktree
    let e = owned(Mode::Follow("main"));
    for (busy, recorded, by) in [
        (true, false, HeldBy::Busy),
        (true, true, HeldBy::Busy),
        (false, true, HeldBy::UnprobedWorktree),
    ] {
        assert_eq!(
            classify_missing(&e, busy, recorded, &[]),
            ClassifiedMissing {
                clone: CloneVerdict::Held {
                    recipe: owned_recipe.clone(),
                    by
                },
                needs_human: vec![],
            }
        );
    }
    // another entry naming its repo holds it for a person, named before
    // anything else
    let shared = Entry {
        same_repo_as: Some("app_wt".into()),
        ..owned(Mode::Follow("main"))
    };
    for (busy, recorded) in [(false, false), (true, false), (false, true), (true, true)] {
        assert_eq!(
            classify_missing(&shared, busy, recorded, &[]),
            ClassifiedMissing {
                clone: CloneVerdict::Held {
                    recipe: owned_recipe.clone(),
                    by: HeldBy::Entry
                },
                needs_human: vec![NeedsHuman::CloneSharesRepo {
                    with: "app_wt".into()
                }],
            },
            "{busy} {recorded}"
        );
    }
    assert!(
        NeedsHuman::CloneSharesRepo {
            with: "app_wt".into()
        }
        .holds_entry()
    );
}

#[test]
fn archived_and_pinned_verdicts() {
    let f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Ahead(1)).unique(1),
            b("feat", O, true, Track::Behind(2)),
        ],
    );
    let archived = Entry {
        archived: true,
        ..owned(Mode::Follow("main"))
    };
    assert_eq!(
        verdicts(&archived, &f),
        named(&[
            ("main", needs(BranchNeedsHuman::ArchivedAhead)),
            // the host serves reads, so behind still fast-forwards
            ("feat", act(SyncAction::FastForward { commits: 2 })),
        ])
    );
    // a pin is never fetched, so its commits are local work, and every
    // other action is the pin's to hold
    assert_eq!(
        verdicts(&owned(Mode::Pinned), &f),
        named(&[
            ("main", Verdict::LocalOnly),
            (
                "feat",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 2 },
                    by: HeldBy::Pinned,
                },
            ),
        ])
    );
}

#[test]
fn shallow_verdicts() {
    let mut f = facts(
        on("main"),
        &[
            b(
                "moved",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 1,
                },
            ),
            b(
                "stranded",
                O,
                true,
                Track::Diverged {
                    ahead: 2,
                    behind: 1,
                },
            )
            .unique(1),
        ],
    );
    f.layout.shallow = true;
    assert_eq!(
        verdicts(&owned(Mode::Head), &f),
        named(&[
            ("moved", act(SyncAction::Move)),
            ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
        ])
    );
}

#[test]
fn third_party_local_work_is_local_only() {
    let f = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("audit", None, false, Track::Even).unique(2)],
    );
    assert_eq!(
        verdicts(&third_party(Mode::Head), &f),
        named(&[("audit", Verdict::LocalOnly)])
    );
}

#[test]
fn shallow_relations_never_count_across_roots() {
    let mut f = facts(
        on("main"),
        &[
            // tips match
            b("main", O, true, Track::Even),
            // origin moved: git says ahead 1 behind 1, the root subtracted
            b(
                "moved",
                O,
                true,
                Track::Diverged {
                    ahead: 1,
                    behind: 1,
                },
            ),
            // a local commit on the fetched tip
            b("on-tip", O, true, Track::Ahead(1)).unique(1).on_tip(),
            // a local commit on the old root, origin moved
            b(
                "stranded",
                O,
                true,
                Track::Diverged {
                    ahead: 2,
                    behind: 1,
                },
            )
            .unique(1),
            b("gone", O, true, Track::Gone),
        ],
    );
    f.layout.shallow = true;
    let got = relations(&owned(Mode::Follow("main")), &f);
    assert_eq!(
        got.into_iter().map(|(_, r)| r).collect::<Vec<_>>(),
        [
            Relation::InSync,
            Relation::Shallow,
            Relation::Ahead { commits: 1 },
            Relation::Shallow,
            Relation::Gone,
        ]
    );
}

#[test]
fn third_party_keeps_only_local_work() {
    let f = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[
            b("main", O, true, Track::Behind(57)),
            b("tsv-format-audit", None, false, Track::Even).unique(3),
            b("master", O, true, Track::Gone),
        ],
    );
    let c = classify(
        &third_party(Mode::Pinned),
        &f,
        &EntrySessions::idle(),
        Refresh::Unasked,
    );
    assert_eq!(c.branches.len(), 1);
    assert_eq!(c.branches[0].name, "tsv-format-audit");
    assert_eq!(c.branches[0].relation, Relation::Untracked);
    assert_eq!(c.branches[0].unique_commits, 3);
    assert_eq!(c.branches[0].newest_commit_at, NOW - 3600);
    // origin is the registry url in SSH form for the owned fixture; the
    // third-party url differs
    assert!(matches!(
        c.needs_human[..],
        [NeedsHuman::OriginMismatch { .. }]
    ));
}

#[test]
fn follow_mode_reasons() {
    let e = owned(Mode::Follow("main"));
    let missing = facts(on("dev"), &[b("dev", O, true, Track::Even)]);
    assert_eq!(
        classify(&e, &missing, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        [NeedsHuman::DefaultBranchMissing {
            branch: "main".into()
        }]
    );
    let no_upstream = facts(on("main"), &[b("main", None, false, Track::Even)]);
    assert_eq!(
        classify(&e, &no_upstream, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        [NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into()
        }]
    );
    let other_remote = facts(
        on("main"),
        &[b("main", Some("upstream"), true, Track::Even)],
    );
    assert_eq!(
        classify(&e, &other_remote, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        [NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into()
        }]
    );
    // unmapped is a branch-level reason, not a missing upstream
    let unmapped = facts(on("main"), &[b("main", O, false, Track::Even)]);
    assert!(
        classify(&e, &unmapped, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );
    let detached = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("main", O, true, Track::Even)],
    );
    assert_eq!(
        classify(&e, &detached, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        [NeedsHuman::UnexpectedDetached {
            checkout: "/ws/app".into()
        }]
    );
    // on a feature branch is not a finding
    let feature = facts(
        on("feat"),
        &[
            b("main", O, true, Track::Even),
            b("feat", O, true, Track::Even),
        ],
    );
    assert!(
        classify(&e, &feature, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );
}

/// The branch an entry follows, its upstream gone from origin (the
/// remote's default renamed), needs a person — never cleanup, never a
/// worktree to remove — and the branch itself reads as one with no
/// upstream does. Any other gone branch stays cleanup.
#[test]
fn a_followed_branch_whose_upstream_is_gone_needs_a_human() {
    let e = owned(Mode::Follow("master"));
    let reason = [NeedsHuman::DefaultBranchGone {
        branch: "master".into(),
    }];
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("master", O, true, Track::Gone),
            b("old", O, true, Track::Gone),
        ],
    );
    // the followed branch in a clean linked worktree, as removable as
    // `old` would be
    f.worktrees = vec![linked("/ws/app-master", on("master"))];
    let c = classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked);
    assert_eq!(c.needs_human, reason);
    assert!(!reason[0].holds_entry());
    assert_eq!(
        verdicts(&e, &f),
        named(&[
            ("main", Verdict::Quiet),
            ("master", Verdict::Quiet),
            (
                "old",
                Verdict::Cleanup {
                    reason: CleanupReason::UpstreamGone,
                    removable_worktree: None,
                }
            ),
        ])
    );
    assert_eq!(branch_relation(&c, "master"), Relation::Gone);
    // with commits on no remote, it's local work, never deletable
    let unique = facts(on("master"), &[b("master", O, true, Track::Gone).unique(2)]);
    assert_eq!(
        classify(&e, &unique, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        reason
    );
    assert_eq!(
        verdicts(&e, &unique),
        named(&[("master", Verdict::LocalOnly)])
    );
    // following another branch, it's cleanup as ever
    let main = owned(Mode::Follow("main"));
    let gone = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("master", O, true, Track::Gone),
        ],
    );
    assert!(
        classify(&main, &gone, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );
    assert!(matches!(
        verdicts(&main, &gone)[1].1,
        Verdict::Cleanup { .. }
    ));
    // a pin's refs are stale by contract, and a reference the run
    // doesn't refresh is compared against no remote: neither says it
    for e in [
        owned(Mode::PinnedOn("master")),
        third_party(Mode::Follow("master")),
    ] {
        let c = classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked);
        assert!(
            !c.needs_human
                .iter()
                .any(|r| matches!(r, NeedsHuman::DefaultBranchGone { .. })),
            "{:?}",
            c.needs_human
        );
    }
    // a reference the run refreshes does
    let mut refreshed = f;
    refreshed.config.origin_urls = vec![OriginUrl::repo("https://github.com/them/lib")];
    refreshed.config.origin_fetch_url = Some("https://github.com/them/lib".into());
    refreshed.push_urls = None;
    let lib = third_party(Mode::Follow("master"));
    assert_eq!(
        classify(&lib, &refreshed, &EntrySessions::idle(), Refresh::Named).needs_human,
        reason
    );
}

/// With `worktrees/` itself unreadable, or an unprobed worktree whose
/// HEAD is unknown, any branch may be checked out where no one can
/// see: none is cleanup. A gone worktree whose HEAD reads doesn't
/// withhold it.
#[test]
fn an_unreadable_worktrees_dir_withholds_cleanup() {
    let e = owned(Mode::Follow("main"));
    let mut f = facts(
        on("main"),
        &[
            b("main", O, true, Track::Even),
            b("gone", O, true, Track::Gone),
            b("gone-work", O, true, Track::Gone).unique(1),
            b("merged", None, false, Track::Even),
        ],
    );
    f.unreadable = vec!["/ws/app/.git/worktrees".into()];
    assert_eq!(
        verdicts(&e, &f),
        named(&[
            ("main", Verdict::Quiet),
            ("gone", Verdict::Quiet),
            ("gone-work", Verdict::LocalOnly),
            ("merged", Verdict::Quiet),
        ])
    );
    let withheld = verdicts(&e, &f);
    // one worktree git dir unreadable: its worktree's HEAD is unknown
    f.unreadable = vec!["/ws/app/.git/worktrees/x".into()];
    f.unprobed = vec![UnprobedWorktree {
        head: UnprobedHead::Unknown,
        ..unprobed(
            "/ws/app/.git/worktrees/x",
            None,
            UnprobedWhy::Failed {
                error: "not listed by git".into(),
            },
        )
    }];
    assert_eq!(verdicts(&e, &f), withheld);
    // a readable worktree git dir whose HEAD isn't
    f.unreadable.clear();
    assert_eq!(verdicts(&e, &f), withheld);
    // a gone worktree on a known branch: cleanup as ever
    f.unprobed = vec![unprobed("/ws/app-x", Some("main"), UnprobedWhy::Prunable)];
    let cleanup = verdicts(&e, &f)
        .into_iter()
        .filter(|(_, v)| matches!(v, Verdict::Cleanup { .. }))
        .count();
    assert_eq!(cleanup, 3);
}

fn branch_relation(c: &Classified, name: &str) -> Relation {
    c.branches.iter().find(|b| b.name == name).unwrap().relation
}

#[test]
fn pinned_and_head_modes() {
    let detached = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("main", O, true, Track::Behind(1))],
    );
    let on_main = facts(on("main"), &[b("main", O, true, Track::Even)]);
    let pinned = owned(Mode::Pinned);
    let head = owned(Mode::Head);
    // a pin's HEAD is its consumer's: detached or on a branch, nothing
    // to say
    for f in [&detached, &on_main] {
        assert!(
            classify(&pinned, f, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
    }
    assert!(
        classify(&head, &detached, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );
    assert!(
        classify(&head, &on_main, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );
}

#[test]
fn a_pin_on_its_branch_expects_nothing_of_it() {
    let pinned = owned(Mode::PinnedOn("fork"));
    let reasons =
        |f: &RepoFacts| classify(&pinned, f, &EntrySessions::idle(), Refresh::Unasked).needs_human;
    // on its branch, behind a stale remote-tracking ref: held, not
    // reported
    let on_fork = facts(on("fork"), &[b("fork", O, true, Track::Behind(3))]);
    assert!(reasons(&on_fork).is_empty());
    assert_eq!(
        verdicts(&pinned, &on_fork),
        named(&[(
            "fork",
            Verdict::Held {
                action: SyncAction::FastForward { commits: 3 },
                by: HeldBy::Pinned,
            },
        )])
    );
    // detached, on another branch, its branch missing or with no
    // upstream: none of the follow reasons
    let detached = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("fork", O, true, Track::Even)],
    );
    let elsewhere = facts(on("main"), &[b("main", O, true, Track::Even)]);
    let no_upstream = facts(on("fork"), &[b("fork", None, false, Track::Even)]);
    for f in [&detached, &elsewhere, &no_upstream] {
        assert!(reasons(f).is_empty());
    }
    // the same branch, followed rather than pinned, has all three
    let followed = owned(Mode::Follow("fork"));
    let followed_reasons = |f: &RepoFacts| {
        classify(&followed, f, &EntrySessions::idle(), Refresh::Unasked).needs_human
    };
    assert!(matches!(
        followed_reasons(&detached)[..],
        [NeedsHuman::UnexpectedDetached { .. }]
    ));
    assert!(matches!(
        followed_reasons(&elsewhere)[..],
        [NeedsHuman::DefaultBranchMissing { .. }]
    ));
    assert!(matches!(
        followed_reasons(&no_upstream)[..],
        [NeedsHuman::DefaultBranchNoUpstream { .. }]
    ));
    // its branch with nothing unique and no upstream isn't merged
    // cleanup: it's where the pin lives
    let detached_bare = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("fork", None, false, Track::Even)],
    );
    assert_eq!(
        verdicts(&pinned, &detached_bare),
        named(&[("fork", Verdict::Quiet)])
    );
}

#[test]
fn a_stale_main_beside_a_pin_is_held_by_it() {
    let held = |commits| Verdict::Held {
        action: SyncAction::FastForward { commits },
        by: HeldBy::Pinned,
    };
    // detached at the pin, and on the pin's branch: a local main far
    // behind a stale origin/main never moves, and local work stays
    // local
    for head in [
        Head::Detached {
            commit: "abc".into(),
        },
        on("fork"),
    ] {
        let f = facts(
            head,
            &[
                b("fork", O, true, Track::Even),
                b("main", O, true, Track::Behind(57)),
                b("audit", None, false, Track::Even).unique(2),
                b("ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        let c = classify(
            &owned(Mode::PinnedOn("fork")),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        );
        assert!(c.needs_human.is_empty());
        assert_eq!(
            c.branches
                .iter()
                .map(|b| (b.name.clone(), b.verdict.clone()))
                .collect::<Vec<_>>(),
            named(&[
                ("fork", Verdict::Quiet),
                ("main", held(57)),
                ("audit", Verdict::LocalOnly),
                ("ahead", Verdict::LocalOnly),
            ])
        );
    }
}

#[test]
fn a_pin_branch_ahead_reads_its_unique_commits() {
    // a fork's main fast-forwarded from upstream: ahead of a stale
    // origin/main with every commit on a remote ref, so nothing local
    let f = facts(
        on("fork"),
        &[
            b("fork", O, true, Track::Even),
            b("main", O, true, Track::Ahead(2)),
            b("work", O, true, Track::Ahead(2)).unique(1),
        ],
    );
    assert_eq!(
        verdicts(&owned(Mode::PinnedOn("fork")), &f),
        named(&[
            ("fork", Verdict::Quiet),
            ("main", Verdict::Quiet),
            ("work", Verdict::LocalOnly),
        ])
    );
}

#[test]
fn a_pin_gets_no_verdict_from_its_stale_refs() {
    let pinned = owned(Mode::PinnedOn("fork"));
    let diverged = Track::Diverged {
        ahead: 1,
        behind: 1,
    };
    let f = facts(
        on("fork"),
        &[
            b("fork", O, true, Track::Gone).unique(1),
            b("side", O, true, Track::Gone),
            b("diverged", O, true, diverged).unique(1),
            b("unmapped", O, false, Track::Even).unique(5),
            b("merged", None, false, Track::Even),
            b("ahead", O, true, Track::Ahead(2)).unique(2),
            b("behind", O, true, Track::Behind(3)),
        ],
    );
    let held = |action| Verdict::Held {
        action,
        by: HeldBy::Pinned,
    };
    // never fetched, so no ref's word is taken: local work where the
    // branch has commits on no remote, else nothing — not the gone
    // branch the pin lives on to clean up, nor a merged one
    assert_eq!(
        verdicts(&pinned, &f),
        named(&[
            ("fork", Verdict::LocalOnly),
            ("side", Verdict::Quiet),
            ("diverged", Verdict::LocalOnly),
            ("unmapped", Verdict::LocalOnly),
            ("merged", Verdict::Quiet),
            ("ahead", Verdict::LocalOnly),
            ("behind", held(SyncAction::FastForward { commits: 3 })),
        ])
    );
    // followed, the same refs are taken at their word — the followed
    // branch's gone upstream its entry's reason, never cleanup
    let gone = |removable_worktree| Verdict::Cleanup {
        reason: CleanupReason::UpstreamGone,
        removable_worktree,
    };
    assert_eq!(
        verdicts(&owned(Mode::Follow("fork")), &f),
        named(&[
            ("fork", Verdict::LocalOnly),
            ("side", gone(None)),
            ("diverged", needs(BranchNeedsHuman::Diverged)),
            ("unmapped", needs(BranchNeedsHuman::Unmapped)),
            (
                "merged",
                Verdict::Cleanup {
                    reason: CleanupReason::Merged,
                    removable_worktree: None,
                },
            ),
            ("ahead", act(SyncAction::Push { commits: 2 })),
            ("behind", act(SyncAction::FastForward { commits: 3 })),
        ])
    );
    // shallow: the stranded work is local, the stale pointer held
    let mut f = facts(
        on("fork"),
        &[
            b("moved", O, true, diverged),
            b("stranded", O, true, diverged).unique(1),
        ],
    );
    f.layout.shallow = true;
    assert_eq!(
        verdicts(&pinned, &f),
        named(&[
            ("moved", held(SyncAction::Move)),
            ("stranded", Verdict::LocalOnly),
        ])
    );
    assert_eq!(
        verdicts(&owned(Mode::Follow("fork")), &f),
        named(&[
            ("moved", act(SyncAction::Move)),
            ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
        ])
    );
}

#[test]
fn a_pin_names_the_hold_before_busy_dirt_and_the_entry() {
    let ff = SyncAction::FastForward { commits: 2 };
    let mut f = facts(
        on("fork"),
        &[
            b("fork", O, true, Track::Behind(2)),
            b("local", O, true, Track::Ahead(1)).unique(1),
        ],
    );
    f.status.uncommitted.unstaged = 1;
    let pinned = owned(Mode::PinnedOn("fork"));
    let followed = owned(Mode::Follow("fork"));
    let busy = busy_at(&["/ws/app"]);
    // followed: the busy dirty checkout names the hold, and holds the
    // push
    assert_eq!(
        verdicts_with(&followed, &f, &busy),
        [
            Verdict::Held {
                action: ff,
                by: HeldBy::Busy
            },
            act(SyncAction::Push { commits: 1 }),
        ]
    );
    // pinned: the pin, which clearing the session or the dirt never
    // releases; its commits stay local work
    let pinned_verdicts = [
        Verdict::Held {
            action: ff,
            by: HeldBy::Pinned,
        },
        Verdict::LocalOnly,
    ];
    assert_eq!(verdicts_with(&pinned, &f, &busy), pinned_verdicts);
    assert_eq!(
        verdicts_with(&pinned, &f, &EntrySessions::idle()),
        pinned_verdicts
    );
    // busy detection unavailable too
    assert_eq!(
        verdicts_with(&pinned, &f, &EntrySessions::unavailable()),
        pinned_verdicts
    );
    // an entry-level reason stays on the entry, the pin still named
    f.in_progress = Some(InProgressOp::Merge);
    let c = classify(&pinned, &f, &busy, Refresh::Unasked);
    assert!(matches!(
        c.needs_human[..],
        [NeedsHuman::OperationInProgress { .. }]
    ));
    assert_eq!(
        c.branches
            .into_iter()
            .map(|b| b.verdict)
            .collect::<Vec<_>>(),
        pinned_verdicts
    );
}

#[test]
fn in_progress_and_origin_reasons() {
    let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
    f.in_progress = Some(InProgressOp::Rebase);
    f.config.origin_urls.clear();
    f.config.origin_keys = OriginKeys::None;
    assert_eq!(
        classify(
            &owned(Mode::Follow("main")),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked
        )
        .needs_human,
        [
            NeedsHuman::OperationInProgress {
                checkout: "/ws/app".into(),
                op: InProgressOp::Rebase
            },
            NeedsHuman::OriginMismatch {
                origin: OriginRemote::Missing,
                expected: "git@github.com:me/app".into(),
                fix: OriginFix::Add,
            },
        ]
    );
    // an `origin` with keys but no URL, the repo's own
    f.config.origin_keys = OriginKeys::InRepo;
    assert!(
        classify(
            &owned(Mode::Follow("main")),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked
        )
        .needs_human
        .contains(&NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/app".into(),
            fix: OriginFix::SetUrl,
        })
    );
    // `origin` only in global config: no `git remote` command can edit it
    // there, and `remote add` would add a URL after it
    f.config.origin_keys = OriginKeys::Elsewhere;
    f.config.origin_urls = vec![OriginUrl::elsewhere("git@github.com:old/app")];
    let reason = |f: &RepoFacts| {
        classify(
            &owned(Mode::Follow("main")),
            f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        )
        .needs_human
        .into_iter()
        .find(|r| matches!(r, NeedsHuman::OriginMismatch { .. }))
    };
    assert_eq!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:old/app".into()
            },
            expected: "git@github.com:me/app".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile
            },
        })
    );
    // `origin` known only through a global fetch refspec: `remote add`
    f.config.origin_urls.clear();
    assert!(matches!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            fix: OriginFix::Add,
            ..
        })
    ));
}

#[test]
fn origin_urls_as_git_reads_them() {
    let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
    let reason = |f: &RepoFacts| {
        classify(
            &owned(Mode::Follow("main")),
            f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        )
        .needs_human
        .into_iter()
        .find(|r| matches!(r, NeedsHuman::OriginMismatch { .. }))
    };
    // read where git connects, never by the text: a lookalike is drift
    for lookalike in LOOKALIKE_URLS {
        f.config.origin_urls = vec![OriginUrl::repo(lookalike)];
        assert!(reason(&f).is_some(), "{lookalike}");
    }
    // an uppercase host is still the registry's
    f.config.origin_urls = vec![OriginUrl::repo("git@GITHUB.com:me/app")];
    assert_eq!(reason(&f), None);
    // the first URL wins: a mismatch first, the registry's second, is
    // still a mismatch — and the other way round isn't
    f.config.origin_urls = vec![
        OriginUrl::repo("https://me:ghp_TOKEN@github.com/old/app"),
        OriginUrl::repo("git@github.com:me/app"),
    ];
    assert_eq!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            // the credential never reaches the report
            origin: OriginRemote::Url {
                url: "https://***@github.com/old/app".into()
            },
            expected: "git@github.com:me/app".into(),
            // several URLs: no command fits every shape of them
            fix: OriginFix::ByHand {
                reason: OriginByHand::SeveralUrls
            },
        })
    );
    f.config.origin_urls.reverse();
    assert_eq!(reason(&f), None);
    // an empty value resets the list: nothing left is no URL, and the
    // reset is beyond what `set-url` can reason about
    f.config.origin_urls = vec![
        OriginUrl::repo("git@github.com:me/app"),
        OriginUrl::repo(""),
    ];
    assert_eq!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/app".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::EmptyValue
            },
        })
    );
    // a reset then a mismatch: that one is what git fetches from
    f.config
        .origin_urls
        .push(OriginUrl::repo("git@github.com:old/app"));
    assert!(matches!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url { .. },
            fix: OriginFix::ByHand {
                reason: OriginByHand::EmptyValue
            },
            ..
        })
    ));
    // a single empty value: `set-url` replaces it; a valueless one
    // breaks every git remote command
    f.config.origin_urls = vec![OriginUrl::repo("")];
    assert_eq!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/app".into(),
            fix: OriginFix::SetUrl,
        })
    );
    f.config.origin_urls = vec![OriginUrl::valueless()];
    assert!(matches!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            fix: OriginFix::ByHand {
                reason: OriginByHand::ValuelessUrl
            },
            ..
        })
    ));
    // one URL in the repo's file: a plain `set-url`
    f.config.origin_urls = vec![OriginUrl::repo("git@github.com:old/app.git")];
    assert!(matches!(
        reason(&f),
        Some(NeedsHuman::OriginMismatch {
            fix: OriginFix::SetUrl,
            ..
        })
    ));
}

#[test]
fn an_operation_in_progress_owns_a_detached_head() {
    let mut f = facts(
        Head::Detached {
            commit: "abc".into(),
        },
        &[b("main", O, true, Track::Even)],
    );
    for op in [InProgressOp::Rebase, InProgressOp::Bisect] {
        f.in_progress = Some(op);
        assert_eq!(
            classify(
                &owned(Mode::Follow("main")),
                &f,
                &EntrySessions::idle(),
                Refresh::Unasked
            )
            .needs_human,
            [NeedsHuman::OperationInProgress {
                checkout: "/ws/app".into(),
                op
            }]
        );
    }
    // the rest don't detach HEAD, so a detach beside one is its own reason
    for op in [
        InProgressOp::Merge,
        InProgressOp::CherryPick,
        InProgressOp::Revert,
        InProgressOp::Sequencer,
        InProgressOp::Am,
    ] {
        f.in_progress = Some(op);
        assert_eq!(
            classify(
                &owned(Mode::Follow("main")),
                &f,
                &EntrySessions::idle(),
                Refresh::Unasked
            )
            .needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op
                },
                NeedsHuman::UnexpectedDetached {
                    checkout: "/ws/app".into()
                },
            ]
        );
    }
}

#[test]
fn a_worktree_that_is_a_registry_dir_is_never_removable() {
    let mut f = facts(on("main"), &[b("old", O, true, Track::Gone).unique(0)]);
    let mut wt = linked("/ws/app-old", on("old"));
    wt.submodules = Some(false);
    f.worktrees = vec![wt];
    let verdict = |f: &RepoFacts| {
        classify(
            &owned(Mode::Follow("main")),
            f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        )
        .branches[0]
            .verdict
            .clone()
    };
    assert_eq!(
        verdict(&f),
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: Some("/ws/app-old".into()),
        }
    );
    f.registry_worktrees.insert("/ws/app-old".into());
    assert_eq!(
        verdict(&f),
        Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: None,
        }
    );
}

#[test]
fn each_checkout_with_an_operation_is_a_reason() {
    let mut f = facts(on("main"), &[b("main", O, true, Track::Ahead(1)).unique(1)]);
    let mut rebasing = linked(
        "/ws/app-rebasing",
        Head::Detached {
            commit: "abc".into(),
        },
    );
    rebasing.in_progress = Some(InProgressOp::Rebase);
    let mut merging = linked("/ws/app-merging", on("feat"));
    merging.in_progress = Some(InProgressOp::Merge);
    f.worktrees = vec![linked("/ws/app-quiet", on("other")), rebasing, merging];
    // gone from disk, but its git dir says a revert is mid-way
    let mut reverting = unprobed("/media/usb/app", None, UnprobedWhy::Missing);
    reverting.in_progress = Some(InProgressOp::Revert);
    f.unprobed = vec![reverting];
    f.in_progress = Some(InProgressOp::CherryPick);
    let c = classify(
        &owned(Mode::Follow("main")),
        &f,
        &EntrySessions::idle(),
        Refresh::Unasked,
    );
    assert_eq!(
        c.needs_human,
        [
            NeedsHuman::OperationInProgress {
                checkout: "/ws/app".into(),
                op: InProgressOp::CherryPick
            },
            NeedsHuman::OperationInProgress {
                checkout: "/ws/app-rebasing".into(),
                op: InProgressOp::Rebase
            },
            NeedsHuman::OperationInProgress {
                checkout: "/ws/app-merging".into(),
                op: InProgressOp::Merge
            },
            NeedsHuman::OperationInProgress {
                checkout: "/media/usb/app".into(),
                op: InProgressOp::Revert
            },
        ]
    );

    // a linked worktree's operation alone holds the entry
    f.in_progress = None;
    f.unprobed.clear();
    f.worktrees.remove(2);
    assert_eq!(
        verdicts(&owned(Mode::Follow("main")), &f),
        named(&[(
            "main",
            Verdict::Held {
                action: SyncAction::Push { commits: 1 },
                by: HeldBy::Entry
            }
        )])
    );
}

#[test]
fn only_the_primary_counts_for_a_detached_head() {
    let e = owned(Mode::Follow("main"));
    // a linked worktree detached is normal
    let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
    f.worktrees = vec![linked(
        "/ws/app-detached",
        Head::Detached {
            commit: "abc".into(),
        },
    )];
    assert!(
        classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked)
            .needs_human
            .is_empty()
    );

    // a rebase in a linked worktree doesn't explain the primary's detach
    f.status.head = Head::Detached {
        commit: "abc".into(),
    };
    f.worktrees[0].in_progress = Some(InProgressOp::Rebase);
    assert_eq!(
        classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked).needs_human,
        [
            NeedsHuman::OperationInProgress {
                checkout: "/ws/app-detached".into(),
                op: InProgressOp::Rebase
            },
            NeedsHuman::UnexpectedDetached {
                checkout: "/ws/app".into()
            },
        ]
    );
}

#[test]
fn origin_normalization() {
    let u = url("https://github.com/Me/App");
    for same in [
        "git@github.com:me/app",
        "git@github.com:me/app.git",
        "ssh://git@github.com/me/app.git",
        "https://github.com/me/app/",
        "https://token@github.com/me/app",
        "git://github.com/me/app.git",
        "git+ssh://github.com/me/app",
        "ssh+git://git@github.com/me/app",
        "git@GitHub.COM:ME/APP.git",
    ] {
        assert!(origin_matches(same, &u), "{same}");
    }
    for different in [
        "git@github.com:me/other",
        "git@gitlab.com:me/app",
        "https://github.com/them/app",
        "git://github.com/them/app",
        "git@github.com:me/app/extra",
        "git@github.com:me",
        "git@github.com:/me/app",
        " git@github.com:me/app",
        "git@github.com:me/app\n",
    ] {
        assert!(!origin_matches(different, &u), "{different}");
    }
}

/// URLs that name the registry's repo somewhere in their text while git
/// connects elsewhere, or that name it in a way the registry's never
/// does: none matches, as origin or push URL.
const LOOKALIKE_URLS: [&str; 20] = [
    // an `@` past the authority: git connects to `evil.com`
    "ssh://evil.com/x@github.com/me/app",
    "evil.com:x@github.com/me/app",
    "ssh+git://evil.com/@github.com/me/app",
    "git@evil.com:git@github.com:me/app",
    "https://evil.com/x@github.com/me/app",
    // escapes git decodes before splitting: `evil.com` again
    "ssh://evil.com%2F@github.com/me/app",
    "ssh://git%40evil.com@github.com/me/app",
    "git@github.com:me%2Fapp",
    // an `@` left in the host
    "ssh://a@b@github.com/me/app",
    // IP literals and brackets
    "git@[::1]:me/app",
    "ssh://git@[::1]/me/app",
    "[git@github.com]:me/app",
    // a bracketed run in the user: git connects to `evil.com`
    "ssh://[evil.com]x@github.com/me/app",
    "ssh://[evil.com]x@github.com:2222/me/app",
    "[evil.com]x@github.com:me/app",
    // `@[` in the path: git's scan runs into it, connecting to `x`
    "git@github.com:me/app@[x]:y",
    // a port: the registry's URLs name none
    "ssh://git@github.com:22/me/app",
    "https://github.com:443/me/app",
    // a user that reads as an option, a scheme git spells otherwise
    "-oProxyCommand=x@github.com:me/app",
    "SSH://git@github.com/me/app",
];

#[test]
fn remote_accounts() {
    for (url, account) in [
        ("git@github.com:Me/app", Some("me")),
        ("git@github.com:me/app.git", Some("me")),
        ("ssh://git@github.com/me/app.git", Some("me")),
        ("ssh://git@host:2222/me/app", Some("me")),
        ("https://token@github.com/them/app/", Some("them")),
        ("https://gitlab.com/group/sub/app", Some("group")),
        ("gh:me/app", Some("me")),
        ("git://github.com/Me/app.git", Some("me")),
        ("/home/me/dev/app", None),
        ("../app", None),
        ("./me/app", None),
        ("file:///srv/git/app.git", None),
        ("https://github.com/me", None),
        // brackets: git connects to another host
        ("ssh://[evil.com]x@github.com:2222/me/app", None),
        ("git@github.com:me/app@[x]:y", None),
        ("", None),
    ] {
        assert_eq!(remote_account(url).as_deref(), account, "{url}");
    }
}
