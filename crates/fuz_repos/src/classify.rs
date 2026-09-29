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
    BranchNeedsHuman, BranchStatus, CleanupReason, Head, HeldBy, InProgressOp, Prune, PruneLoss,
    Relation, SyncAction, UnprobedHead, UnprobedWhy, UnprobedWorktree, UnprobedWorktreeStatus,
    Verdict,
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
    /// A worktree's git dir (or `<commondir>/worktrees/` itself) can't be
    /// read, so whether an operation is in progress there is unknowable.
    WorktreeUnreadable {
        path: String,
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
    /// branch's action: an operation mid-way owns the checkout (and one that
    /// can't be ruled out counts the same), and a wrong origin would move
    /// branches to another repo's history. The rest concern
    /// one branch or the checkout's HEAD, and leave the other branches safe
    /// to sync.
    pub const fn holds_entry(&self) -> bool {
        match self {
            Self::NotARepo { .. }
            | Self::OperationInProgress { .. }
            | Self::OriginMismatch { .. }
            | Self::WorktreeUnreadable { .. } => true,
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
    /// The repo's unprobed worktrees, each with what pruning it would do.
    pub unprobed: Vec<UnprobedWorktreeStatus>,
}

/// What `git worktree prune` would do to an unprobed worktree.
///
/// `None` unless it's `Prunable` (its dir gone); `Safe` when it loses
/// nothing; else what it loses — an operation's state, or a HEAD that may be
/// the only ref to its commit (detached, unreadable, or on a branch that no
/// longer exists).
fn prune(u: &UnprobedWorktree, facts: &RepoFacts) -> Option<Prune> {
    if u.why != UnprobedWhy::Prunable {
        return None;
    }
    let mut losses = Vec::new();
    if let Some(op) = u.in_progress {
        losses.push(PruneLoss::Operation { op });
    }
    match &u.head {
        UnprobedHead::Branch { name } => {
            if !facts.branches.iter().any(|b| b.branch.name == *name) {
                losses.push(PruneLoss::MissingBranch { name: name.clone() });
            }
        }
        UnprobedHead::Detached { .. } => losses.push(PruneLoss::DetachedHead),
        UnprobedHead::Unknown => losses.push(PruneLoss::UnknownHead),
    }
    Some(if losses.is_empty() {
        Prune::Safe
    } else {
        Prune::Loses { losses }
    })
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
            let on = checkouts_on(b, facts);
            let held = if entry_held {
                Some(HeldBy::Entry)
            } else {
                on.hold()
            };
            let verdict = verdict(entry, b, relation, upstream.is_some(), held, &on);
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
    let unprobed = facts
        .unprobed
        .iter()
        .map(|u| UnprobedWorktreeStatus {
            prune: prune(u, facts),
            worktree: u.clone(),
        })
        .collect();
    Classified {
        branches,
        needs_human,
        unprobed,
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

/// Every checkout a branch is on, folded: git allows one branch on HEAD in
/// several (`worktree add -f`, `checkout --ignore-other-worktrees`), and any
/// one of them dirty or unknown holds it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CheckoutsOn<'a> {
    /// On HEAD in some checkout — or possibly, in one whose HEAD is unknown.
    checked_out: bool,
    dirty: bool,
    unprobed: bool,
    /// The one checkout it's on, when that's a worktree `git worktree
    /// remove` would take: linked (never the main worktree), no submodules,
    /// not locked, no operation in progress, clean.
    removable: Option<&'a str>,
}

impl CheckoutsOn<'_> {
    /// A dirty checkout holds the branch; so does one that couldn't be
    /// probed, its state unknown.
    const fn hold(&self) -> Option<HeldBy> {
        if self.dirty {
            Some(HeldBy::DirtyCheckout)
        } else if self.unprobed {
            Some(HeldBy::UnprobedWorktree)
        } else {
            None
        }
    }
}

/// Folds every checkout whose HEAD is the branch — the primary, each probed
/// worktree, and each unprobed one on it; an unprobed one whose HEAD is
/// unknown might be on any branch, so it counts for all. Matched by name,
/// never by path: `%(worktreepath)` names only one checkout, git's paths are
/// resolved while the primary's is root-joined, and a symlinked workspace
/// root makes them differ.
fn checkouts_on<'a>(b: &BranchFacts, facts: &'a RepoFacts) -> CheckoutsOn<'a> {
    let name = b.branch.name.as_str();
    let on = |head: &Head| matches!(head, Head::Branch { name: n } if n == name);
    let mut folded = CheckoutsOn::default();
    let mut count = 0;
    let mut removable = None;
    if on(&facts.status.head) {
        count += 1;
        folded.dirty |= !facts.status.uncommitted.is_clean();
    }
    for c in facts.worktrees.iter().filter(|c| on(&c.head)) {
        count += 1;
        folded.dirty |= !c.uncommitted.is_clean();
        let removable_here = c.linked
            && c.submodules == Some(false)
            && !c.locked
            && c.in_progress.is_none()
            && c.uncommitted.is_clean();
        if removable_here {
            removable = Some(c.path.as_str());
        }
    }
    let unprobed = facts
        .unprobed
        .iter()
        .filter(|u| match &u.head {
            UnprobedHead::Branch { name: n } => n == name,
            UnprobedHead::Detached { .. } => false,
            UnprobedHead::Unknown => true,
        })
        .count();
    if unprobed > 0 {
        count += unprobed;
        folded.unprobed = true;
    }
    // git says it's checked out, but in no checkout it listed: unknown
    if count == 0 && b.branch.worktree.is_some() {
        folded.unprobed = true;
    }
    folded.checked_out = count > 0 || b.branch.worktree.is_some();
    if count == 1 {
        folded.removable = removable;
    }
    folded
}

/// What sync does with a branch. `has_upstream` is whether any upstream is
/// configured; `held` what, if anything, holds a fast-forward or move back —
/// an entry-level reason holds every action, a checkout all but a push;
/// `on` the checkouts it's on.
fn verdict(
    entry: &Entry,
    b: &BranchFacts,
    relation: Relation,
    has_upstream: bool,
    held: Option<HeldBy>,
    on: &CheckoutsOn<'_>,
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
                removable_worktree: on.removable.map(str::to_owned),
            };
        }
        Relation::Untracked if b.unique_commits > 0 => return Verdict::LocalOnly,
        // nothing unique and no upstream: merged, unless it's checked out
        // anywhere, possibly (a fresh branch looks the same), or the
        // registry's branch (a needs-human reason) — so never in a worktree
        // to remove
        Relation::Untracked
            if !has_upstream && !on.checked_out && Some(b.branch.name.as_str()) != follow =>
        {
            return Verdict::Cleanup {
                reason: CleanupReason::Merged,
                removable_worktree: None,
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
    // one per checkout with an operation mid-way, the primary's first, then
    // the other worktrees', probed or not
    let ops = std::iter::once((&facts.path, facts.in_progress))
        .chain(facts.worktrees.iter().map(|c| (&c.path, c.in_progress)))
        .chain(facts.unprobed.iter().map(|u| (&u.path, u.in_progress)));
    for (checkout, op) in ops {
        if let Some(op) = op {
            reasons.push(NeedsHuman::OperationInProgress {
                checkout: checkout.clone(),
                op,
            });
        }
    }
    // an operation there can't be ruled out
    for path in &facts.unreadable {
        reasons.push(NeedsHuman::WorktreeUnreadable { path: path.clone() });
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
            // a rebase or bisect detaches HEAD by design: the operation is
            // the reason, and reattaching mid-way would be the wrong fix; a
            // merge, cherry-pick, revert, or sequencer keeps HEAD on its
            // branch, so a detach beside one is still unexpected. Only the
            // primary's HEAD and operation count: a linked worktree detached
            // is normal, and its operation can't explain the primary's HEAD
            if matches!(head, Head::Detached { .. })
                && !matches!(
                    facts.in_progress,
                    Some(InProgressOp::Rebase | InProgressOp::Bisect)
                )
            {
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

/// The account a remote URL names, in `origin_matches`'s normalized
/// (lowercased) form: the second segment of `host/account/name…`; `None`
/// for a URL with no host and account, such as a local path.
pub fn remote_account(url: &str) -> Option<String> {
    let normalized = normalize_remote(url);
    let mut parts = normalized.split('/');
    let (Some(host), Some(account), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let named = |s: &str| !s.is_empty() && s != "." && s != "..";
    (named(host) && named(account) && named(name)).then(|| account.to_owned())
}

fn normalize_remote(url: &str) -> String {
    let url = url.trim();
    let scheme_less = ["ssh://", "git://", "https://", "http://"]
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
    use crate::state::{Checkout, Layout, Uncommitted, UnprobedWhy, UnprobedWorktree};

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
            primary_linked: false,
            primary_locked: false,
            worktrees: Vec::new(),
            unprobed: Vec::new(),
            unreadable: Vec::new(),
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
        }
    }

    /// An unprobed worktree at `path`, on `branch`.
    fn unprobed(path: &str, branch: Option<&str>, why: UnprobedWhy) -> UnprobedWorktree {
        UnprobedWorktree {
            path: path.into(),
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
        let e = owned(follow("main"));

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

    #[test]
    fn a_worktree_that_was_not_probed_holds_all_but_a_push() {
        // git says both are checked out, but no probed checkout has them on
        // HEAD: the worktree's dir is gone, or its probe failed
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("gone-wt", O, true, Track::Behind(1)),
                b("gone-wt-ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        f.branches[1].branch.worktree = Some("/ws/app-gone".into());
        f.branches[2].branch.worktree = Some("/ws/app-gone-2".into());
        f.unprobed = vec![
            unprobed("/ws/app-gone", Some("gone-wt"), UnprobedWhy::Prunable),
            unprobed(
                "/ws/app-gone-2",
                Some("gone-wt-ahead"),
                UnprobedWhy::Missing,
            ),
        ];
        // a probed linked worktree on another branch doesn't count
        f.worktrees = vec![linked("/ws/app-other", on("main-2"))];
        assert_eq!(
            verdicts(&owned(follow("main")), &f),
            named(&[
                ("main", Verdict::Quiet),
                (
                    "gone-wt",
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 1 },
                        by: HeldBy::UnprobedWorktree,
                    }
                ),
                ("gone-wt-ahead", act(SyncAction::Push { commits: 1 })),
            ])
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
        // named only by `%(worktreepath)`: fail closed
        f.branches[2].branch.worktree = Some("/ws/app-somewhere".into());
        assert_eq!(
            verdicts(&owned(follow("main")), &f)[1..],
            named(&[("listed", held.clone()), ("unlisted", held)])
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
            verdicts(&owned(follow("main")), &f),
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
                // it might be checked out there too: not the one to remove
                (
                    "gone",
                    Verdict::Cleanup {
                        reason: CleanupReason::UpstreamGone,
                        removable_worktree: None,
                    }
                ),
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
        let c = classify(&owned(follow("main")), &f);
        assert_eq!(
            c.needs_human,
            [NeedsHuman::WorktreeUnreadable {
                path: "/ws/app/.git/worktrees".into()
            }]
        );
        assert_eq!(
            verdicts(&owned(follow("main")), &f),
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
        // (`worktree add -f`); any dirty one holds it
        let ff1 = Verdict::Act {
            action: SyncAction::FastForward { commits: 1 },
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
        let e = owned(follow("main"));
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
        let v = verdicts(&owned(follow("main")), &f);
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
            verdicts(&owned(follow("main")), &f),
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
                classify(&owned(follow("main")), &f).needs_human,
                [NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op
                }]
            );
        }
        // a merge doesn't detach HEAD, so a detach beside one is its own reason
        f.in_progress = Some(InProgressOp::Merge);
        assert_eq!(
            classify(&owned(follow("main")), &f).needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op: InProgressOp::Merge
                },
                NeedsHuman::UnexpectedDetached {
                    checkout: "/ws/app".into()
                },
            ]
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
        let c = classify(&owned(follow("main")), &f);
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
            verdicts(&owned(follow("main")), &f),
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
        let e = owned(follow("main"));
        // a linked worktree detached is normal
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        f.worktrees = vec![linked(
            "/ws/app-detached",
            Head::Detached {
                commit: "abc".into(),
            },
        )];
        assert!(classify(&e, &f).needs_human.is_empty());

        // a rebase in a linked worktree doesn't explain the primary's detach
        f.status.head = Head::Detached {
            commit: "abc".into(),
        };
        f.worktrees[0].in_progress = Some(InProgressOp::Rebase);
        assert_eq!(
            classify(&e, &f).needs_human,
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
        ] {
            assert!(origin_matches(same, &u), "{same}");
        }
        for different in [
            "git@github.com:me/other",
            "git@gitlab.com:me/app",
            "https://github.com/them/app",
            "git://github.com/them/app",
        ] {
            assert!(!origin_matches(different, &u), "{different}");
        }
    }

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
            ("", None),
        ] {
            assert_eq!(remote_account(url).as_deref(), account, "{url}");
        }
    }
}
