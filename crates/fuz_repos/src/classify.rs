//! Probe facts → each branch's relation and verdict, and the entry's
//! `needs_human` reasons. Pure.
//!
//! The verdict is the one place sync's per-branch decision is made: `status`
//! previews it, `sync` executes it, and JSON consumers read it rather than
//! re-deriving policy from relations.

use serde::Serialize;

use crate::porcelain::{BranchConfig, Track};
use crate::probe::{BranchFacts, RepoFacts};
use crate::registry::{CheckoutMode, Entry, RepoUrl};
use crate::state::{
    BranchNeedsHuman, BranchStatus, CleanupReason, Head, HeldBy, InProgressOp, Relation,
    SyncAction, Verdict,
};

/// Why `sync` would stop on an entry and leave it to a person. Branch-level
/// reasons are on each branch's `Verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsHuman {
    /// The dir exists but git finds no repo there; `detail` says why (an
    /// empty dir, or git's message).
    NotARepo {
        detail: String,
    },
    OperationInProgress {
        checkout: String,
        op: InProgressOp,
    },
    /// `origin` isn't the registry's `url`; `None` when there's no `origin`.
    /// `expected` is the URL to set it to (SSH when owned, else HTTPS).
    OriginMismatch {
        origin: Option<String>,
        expected: String,
    },
    DefaultBranchMissing {
        branch: String,
    },
    DefaultBranchNoUpstream {
        branch: String,
    },
    UnexpectedDetached {
        checkout: String,
    },
    PinnedOnBranch {
        branch: String,
    },
}

impl NeedsHuman {
    /// Whether the reason stops sync on the whole entry, holding every
    /// branch's action: an operation mid-way owns the checkout, and a wrong
    /// origin would move branches to another repo's history. The rest concern
    /// one branch or the checkout's HEAD, and leave the other branches safe
    /// to sync.
    pub const fn holds_entry(&self) -> bool {
        match self {
            Self::NotARepo { .. }
            | Self::OperationInProgress { .. }
            | Self::OriginMismatch { .. } => true,
            Self::DefaultBranchMissing { .. }
            | Self::DefaultBranchNoUpstream { .. }
            | Self::UnexpectedDetached { .. }
            | Self::PinnedOnBranch { .. } => false,
        }
    }
}

/// What `classify` derives for a present repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub branches: Vec<BranchStatus>,
    pub needs_human: Vec<NeedsHuman>,
}

/// Classifies a present repo's facts against its registry entry.
///
/// Owned entries get a relation per branch. Third-party references are never
/// compared against a remote: they keep only branches with commits on no
/// remote, as `Untracked` — local work that can never be pushed.
pub fn classify(entry: &Entry, facts: &RepoFacts) -> Classified {
    let needs_human = needs_human(entry, facts);
    let entry_held = needs_human.iter().any(NeedsHuman::holds_entry);
    let branches = facts
        .branches
        .iter()
        .filter_map(|b| {
            let relation = if entry.writable {
                relation(b, facts)
            } else if b.unique_commits > 0 {
                Relation::Untracked
            } else {
                return None;
            };
            let upstream = facts
                .config
                .branches
                .get(&b.branch.name)
                .and_then(BranchConfig::display);
            let held = if entry_held {
                Some(HeldBy::Entry)
            } else {
                checkout_hold(b, facts)
            };
            let verdict = verdict(entry, b, relation, upstream.is_some(), held);
            Some(BranchStatus {
                name: b.branch.name.clone(),
                upstream,
                worktree: b.branch.worktree.clone(),
                unique_commits: b.unique_commits,
                newest_commit_at: b.branch.committer_time,
                relation,
                verdict,
            })
        })
        .collect();
    Classified {
        branches,
        needs_human,
    }
}

/// An owned branch's relation to its origin upstream.
fn relation(b: &BranchFacts, facts: &RepoFacts) -> Relation {
    let origin = facts
        .config
        .branches
        .get(&b.branch.name)
        .is_some_and(BranchConfig::is_origin);
    if !origin {
        return Relation::Untracked;
    }
    if b.branch.upstream_ref.is_none() {
        return Relation::Unmapped;
    }
    match (b.branch.track, facts.layout.shallow) {
        (Track::Gone, _) => Relation::Gone,
        (Track::Even, _) => Relation::InSync,
        (_, true) if b.unique_commits > 0 && b.on_fetched_tip => Relation::Ahead {
            commits: b.unique_commits,
        },
        (_, true) => Relation::Shallow,
        (Track::Ahead(commits), false) => Relation::Ahead { commits },
        (Track::Behind(commits), false) => Relation::Behind { commits },
        (Track::Diverged { ahead, behind }, false) => Relation::Diverged { ahead, behind },
    }
}

/// What the branch's checkout holds back, if it's checked out: the primary
/// checkout when it's dirty, or a linked worktree, whose state isn't probed
/// yet. Matched by name against the primary's HEAD, not by path, so a
/// symlinked workspace root can't make the primary look like a worktree.
fn checkout_hold(b: &BranchFacts, facts: &RepoFacts) -> Option<HeldBy> {
    b.branch.worktree.as_ref()?;
    match &facts.status.head {
        Head::Branch { name } if *name == b.branch.name => {
            (!facts.status.uncommitted.is_clean()).then_some(HeldBy::DirtyCheckout)
        }
        // TODO: read the linked worktree's dirt once pass 2 probes worktrees
        _ => Some(HeldBy::UnprobedWorktree),
    }
}

/// What sync does with a branch. `has_upstream` is whether any upstream is
/// configured; `held` what, if anything, holds a fast-forward or move back —
/// an entry-level reason holds every action, a checkout all but a push.
fn verdict(
    entry: &Entry,
    b: &BranchFacts,
    relation: Relation,
    has_upstream: bool,
    held: Option<HeldBy>,
) -> Verdict {
    let follow = match &entry.checkout_mode {
        CheckoutMode::Follow { branch } => Some(branch.as_str()),
        CheckoutMode::Pinned | CheckoutMode::Head => None,
    };
    let action = match relation {
        Relation::Ahead { .. } if entry.archived => {
            return Verdict::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead,
            };
        }
        Relation::Ahead { commits } => SyncAction::Push { commits },
        Relation::Behind { commits } => SyncAction::FastForward { commits },
        // nothing local at stake: a stale pointer at an old root
        Relation::Shallow if b.unique_commits == 0 => SyncAction::Move,
        Relation::Shallow => {
            return Verdict::NeedsHuman {
                reason: BranchNeedsHuman::ShallowLocalWork,
            };
        }
        Relation::Diverged { .. } => {
            return Verdict::NeedsHuman {
                reason: BranchNeedsHuman::Diverged,
            };
        }
        Relation::Unmapped => {
            return Verdict::NeedsHuman {
                reason: BranchNeedsHuman::Unmapped,
            };
        }
        Relation::Gone => {
            return Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
            };
        }
        Relation::Untracked if b.unique_commits > 0 => return Verdict::LocalOnly,
        // nothing unique and no upstream: merged, unless it's checked out (a
        // fresh branch) or the registry's branch (a needs-human reason)
        Relation::Untracked
            if !has_upstream
                && b.branch.worktree.is_none()
                && Some(b.branch.name.as_str()) != follow =>
        {
            return Verdict::Cleanup {
                reason: CleanupReason::Merged,
            };
        }
        // in sync; tracking another remote; checked out with nothing committed
        Relation::InSync | Relation::Untracked => return Verdict::Quiet,
    };
    // a pinned checkout is never fetched, moved, or pushed; commits it
    // carries are local work
    if entry.checkout_mode == CheckoutMode::Pinned {
        return match action {
            SyncAction::Push { .. } => Verdict::LocalOnly,
            SyncAction::FastForward { .. } | SyncAction::Move => Verdict::Quiet,
        };
    }
    match held {
        // a push only moves refs, so only an entry-level reason holds it
        Some(by) if by == HeldBy::Entry || !matches!(action, SyncAction::Push { .. }) => {
            Verdict::Held { action, by }
        }
        _ => Verdict::Act { action },
    }
}

fn needs_human(entry: &Entry, facts: &RepoFacts) -> Vec<NeedsHuman> {
    let mut reasons = Vec::new();
    if let Some(op) = facts.in_progress {
        reasons.push(NeedsHuman::OperationInProgress {
            checkout: facts.path.clone(),
            op,
        });
    }
    match &facts.config.origin_url {
        Some(origin) if origin_matches(origin, &entry.url) => {}
        origin => reasons.push(NeedsHuman::OriginMismatch {
            origin: origin.clone(),
            expected: entry.remote_url(),
        }),
    }
    let head = &facts.status.head;
    match (&entry.checkout_mode, head) {
        (CheckoutMode::Follow { branch }, _) => {
            if !facts.branches.iter().any(|b| b.branch.name == *branch) {
                reasons.push(NeedsHuman::DefaultBranchMissing {
                    branch: branch.clone(),
                });
            } else if !facts
                .config
                .branches
                .get(branch)
                .is_some_and(BranchConfig::is_origin)
            {
                reasons.push(NeedsHuman::DefaultBranchNoUpstream {
                    branch: branch.clone(),
                });
            }
            if matches!(head, Head::Detached { .. }) {
                reasons.push(NeedsHuman::UnexpectedDetached {
                    checkout: facts.path.clone(),
                });
            }
        }
        (CheckoutMode::Pinned, Head::Branch { name }) => {
            reasons.push(NeedsHuman::PinnedOnBranch {
                branch: name.clone(),
            });
        }
        (CheckoutMode::Pinned, Head::Detached { .. }) | (CheckoutMode::Head, _) => {}
    }
    reasons
}

/// Whether a remote URL names the registry's repo: SSH and HTTPS forms
/// compare equal, `.git` and trailing slashes drop, userinfo drops, and case
/// folds (GitHub paths are case-insensitive).
pub fn origin_matches(origin: &str, url: &RepoUrl) -> bool {
    normalize_remote(origin) == normalize_remote(&url.to_string())
}

fn normalize_remote(url: &str) -> String {
    let url = url.trim();
    let scheme_less = ["ssh://", "https://", "http://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    // otherwise scp-like `git@host:account/name`
    let rest = scheme_less.map_or_else(|| url.replacen(':', "/", 1), str::to_owned);
    let rest = rest.split_once('@').map_or(rest.as_str(), |(_, r)| r);
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    rest.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::porcelain::{ConfigFacts, RefFacts, StatusFacts};
    use crate::registry::EntryKind;
    use crate::state::{Layout, Uncommitted};

    const NOW: u64 = 1_800_000_000;

    fn url(s: &str) -> RepoUrl {
        RepoUrl::try_from(s.to_owned()).unwrap()
    }

    fn owned(mode: CheckoutMode) -> Entry {
        Entry {
            key: "app".into(),
            kind: EntryKind::Repo,
            dir: "app".into(),
            url: url("https://github.com/me/app"),
            writable: true,
            archived: false,
            visibility: None,
            ci: false,
            checkout_mode: mode,
        }
    }

    fn follow(branch: &str) -> CheckoutMode {
        CheckoutMode::Follow {
            branch: branch.into(),
        }
    }

    fn third_party(mode: CheckoutMode) -> Entry {
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
            origin_url: Some("git@github.com:me/app".into()),
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
            git_dir: PathBuf::from("/ws/app/.git"),
            common_dir: PathBuf::from("/ws/app/.git"),
            config,
            status: StatusFacts {
                head,
                uncommitted: Uncommitted::default(),
                stashes: 0,
            },
            in_progress: None,
            branches: branches
                .iter()
                .map(|b| BranchFacts {
                    branch: RefFacts {
                        name: b.name.into(),
                        upstream_ref: b.resolved.then(|| {
                            format!("refs/remotes/{}/{}", b.remote.unwrap_or("origin"), b.name)
                        }),
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
        }
    }

    fn on(name: &str) -> Head {
        Head::Branch { name: name.into() }
    }

    fn relations(entry: &Entry, f: &RepoFacts) -> Vec<(String, Relation)> {
        classify(entry, f)
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
        let got = relations(&owned(follow("main")), &f);
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
        classify(entry, f)
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect()
    }

    fn named(want: &[(&str, Verdict)]) -> Vec<(String, Verdict)> {
        want.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect()
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
            verdicts(&owned(follow("main")), &f),
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
                        reason: CleanupReason::UpstreamGone
                    }
                ),
                ("unmapped", needs(BranchNeedsHuman::Unmapped)),
                ("local", Verdict::LocalOnly),
                (
                    "merged",
                    Verdict::Cleanup {
                        reason: CleanupReason::Merged
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
        let e = owned(follow("main"));

        let mut drift = facts(on("main"), &branches);
        drift.config.origin_url = Some("git@github.com:someone/app".into());
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
        f.branches[0].branch.worktree = Some("/ws/app".into());
        f.branches[2].branch.worktree = Some("/ws/app-linked".into());
        f.branches[3].branch.worktree = Some("/ws/app-linked-2".into());
        let e = owned(follow("main"));
        let unprobed = [
            ("linked", held(ff(1), HeldBy::UnprobedWorktree)),
            // a push only moves refs
            ("linked-ahead", act(SyncAction::Push { commits: 1 })),
        ];

        let clean = verdicts(&e, &f);
        assert_eq!(
            clean[..2],
            named(&[("main", act(ff(2))), ("other", act(ff(3)))])
        );
        // a linked worktree isn't probed yet: unknown, not dirty
        assert_eq!(clean[2..], named(&unprobed));

        f.status.uncommitted.unstaged = 1;
        let dirty = verdicts(&e, &f);
        assert_eq!(
            dirty[..2],
            named(&[
                ("main", held(ff(2), HeldBy::DirtyCheckout)),
                // not checked out: moves in place
                ("other", act(ff(3))),
            ])
        );
        assert_eq!(dirty[2..], named(&unprobed));

        // ahead, checked out in the dirty primary: the push still acts
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
            ..owned(follow("main"))
        };
        assert_eq!(
            verdicts(&archived, &f),
            named(&[
                ("main", needs(BranchNeedsHuman::ArchivedAhead)),
                // the host serves reads, so behind still fast-forwards
                ("feat", act(SyncAction::FastForward { commits: 2 })),
            ])
        );
        // a pinned checkout is never moved or pushed; its commits are local
        // work
        assert_eq!(
            verdicts(&owned(CheckoutMode::Pinned), &f),
            named(&[("main", Verdict::LocalOnly), ("feat", Verdict::Quiet)])
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
            verdicts(&owned(CheckoutMode::Head), &f),
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
            verdicts(&third_party(CheckoutMode::Head), &f),
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
        let got = relations(&owned(follow("main")), &f);
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
        let c = classify(&third_party(CheckoutMode::Pinned), &f);
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
        let e = owned(follow("main"));
        let missing = facts(on("dev"), &[b("dev", O, true, Track::Even)]);
        assert_eq!(
            classify(&e, &missing).needs_human,
            [NeedsHuman::DefaultBranchMissing {
                branch: "main".into()
            }]
        );
        let no_upstream = facts(on("main"), &[b("main", None, false, Track::Even)]);
        assert_eq!(
            classify(&e, &no_upstream).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        let other_remote = facts(
            on("main"),
            &[b("main", Some("upstream"), true, Track::Even)],
        );
        assert_eq!(
            classify(&e, &other_remote).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        // unmapped is a branch-level reason, not a missing upstream
        let unmapped = facts(on("main"), &[b("main", O, false, Track::Even)]);
        assert!(classify(&e, &unmapped).needs_human.is_empty());
        let detached = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("main", O, true, Track::Even)],
        );
        assert_eq!(
            classify(&e, &detached).needs_human,
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
        assert!(classify(&e, &feature).needs_human.is_empty());
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
        let pinned = owned(CheckoutMode::Pinned);
        let head = owned(CheckoutMode::Head);
        assert!(classify(&pinned, &detached).needs_human.is_empty());
        assert_eq!(
            classify(&pinned, &on_main).needs_human,
            [NeedsHuman::PinnedOnBranch {
                branch: "main".into()
            }]
        );
        assert!(classify(&head, &detached).needs_human.is_empty());
        assert!(classify(&head, &on_main).needs_human.is_empty());
    }

    #[test]
    fn in_progress_and_origin_reasons() {
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        f.in_progress = Some(InProgressOp::Rebase);
        f.config.origin_url = None;
        assert_eq!(
            classify(&owned(follow("main")), &f).needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op: InProgressOp::Rebase
                },
                NeedsHuman::OriginMismatch {
                    origin: None,
                    expected: "git@github.com:me/app".into()
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
        ] {
            assert!(origin_matches(same, &u), "{same}");
        }
        for different in [
            "git@github.com:me/other",
            "git@gitlab.com:me/app",
            "https://github.com/them/app",
        ] {
            assert!(!origin_matches(different, &u), "{different}");
        }
    }
}
