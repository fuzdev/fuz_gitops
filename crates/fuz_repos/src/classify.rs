//! Probe facts → each branch's relation and verdict, and the entry's
//! `needs_human` reasons. Pure.
//!
//! The verdict is the one place sync's per-branch decision is made: `status`
//! previews it, `sync` executes it, and JSON consumers read it rather than
//! re-deriving policy from relations.

use serde::Serialize;

use crate::busy::{Detection, EntrySessions};
use crate::porcelain::{BranchConfig, ConfigFacts, OriginKeys, OriginUrl, Track};
use crate::probe::{BranchFacts, RepoFacts};
use crate::registry::{CheckoutMode, Entry, RepoUrl};
use crate::sessions::Session;
use crate::state::{
    BranchNeedsHuman, BranchStatus, CleanupReason, Head, HeldBy, InProgressOp, Prune, PruneLoss,
    Relation, SyncAction, UnprobedHead, UnprobedWhy, UnprobedWorktree, UnprobedWorktreeStatus,
    Verdict,
};
use crate::url::without_userinfo;

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
    /// `origin` isn't the registry's `url`, or has none. `expected` is the
    /// URL it should have (SSH when owned, else HTTPS); `fix`, how to set it
    /// with a command that can.
    OriginMismatch {
        origin: OriginRemote,
        expected: String,
        fix: OriginFix,
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
    /// A checkout's path can't be resolved — `path` is as far as it got, in
    /// a dir the tool can't search or a symlink loop — so whether a live
    /// session works in it can't be told. It may be busy: like a busy
    /// checkout, it holds the branches checked out there (`BusyUnknown`),
    /// and the entry's other branches act.
    CheckoutUnresolvable {
        checkout: String,
        path: String,
        error: String,
    },
    /// A git dir no worktree list names shares the repo's refs — made by
    /// hand, with a `commondir` file naming its common dir, or by
    /// `git-new-workdir`, with a symlinked `refs` — and a live session works
    /// through it, so a commit there moves the branch its HEAD names. It's
    /// held as though busy (`BusyUnknown`), every branch when its HEAD is
    /// unknown, and the entry's other branches act. Seen only with a session
    /// in it: git itself doesn't know it's there.
    UnlistedGitDir {
        git_dir: String,
        head: UnprobedHead,
        busy: Vec<Session>,
    },
}

impl NeedsHuman {
    /// Whether the reason stops sync on the whole entry, holding every
    /// branch's action: an operation mid-way owns the checkout (and one that
    /// can't be ruled out counts the same), and a wrong origin would move
    /// branches to another repo's history. The rest concern one branch or
    /// one checkout's HEAD (an unresolvable checkout holds the branches
    /// checked out there, as a busy one does), and leave the other branches
    /// safe to sync.
    pub const fn holds_entry(&self) -> bool {
        match self {
            Self::NotARepo { .. }
            | Self::OperationInProgress { .. }
            | Self::OriginMismatch { .. }
            | Self::WorktreeUnreadable { .. } => true,
            Self::DefaultBranchMissing { .. }
            | Self::DefaultBranchNoUpstream { .. }
            | Self::UnexpectedDetached { .. }
            | Self::PinnedOnBranch { .. }
            | Self::CheckoutUnresolvable { .. }
            | Self::UnlistedGitDir { .. } => false,
        }
    }
}

/// The `origin` remote as git sees it (every config scope), when it isn't
/// the registry's `url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginRemote {
    /// Another URL: the one git fetches from, the first of the list (an
    /// empty value resets the list). A credential in its userinfo is
    /// redacted as `***`.
    Url { url: String },
    /// Known to git — some `remote.origin.*` key is set — but with no URL,
    /// or a list an empty value reset.
    NoUrl,
    /// No `remote.origin.*` key in any scope.
    Missing,
}

/// How to point `origin` at the registry's URL.
///
/// Decided from what each command can edit: `git remote` writes the repo's
/// own config file, and refuses or accepts by whether the repo's scope
/// configures `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginFix {
    /// `git remote add origin <expected>`: no `remote.origin.*` key in the
    /// repo's scope (`remote set-url` would say `No such remote`).
    Add,
    /// `git remote set-url origin <expected>`: `origin` is the repo's, with
    /// at most one URL, in its own config file.
    SetUrl,
    /// Fix `remote.origin.url` by hand, for `reason`.
    ByHand { reason: OriginByHand },
}

/// Why no `git remote` command fits an origin's fix. When several apply,
/// the first listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginByHand {
    /// A URL git reads from beyond the repo's own file (global or system
    /// config, an included file, the worktree config), which no `git remote`
    /// command edits and which would still come first.
    OutsideRepoFile,
    /// A valueless `url` (no `=`), which breaks every git remote command
    /// (`missing value for 'remote.origin.url'`).
    ValuelessUrl,
    /// An empty value among several: it resets the list, and with several
    /// values `set-url` fails (`remote.origin.url has multiple values`). A
    /// single empty value is `SetUrl`'s to replace.
    EmptyValue,
    /// Several URLs, where a plain `set-url` fails (`remote.origin.url has
    /// multiple values`) and a value-pattern one fails too whenever the first
    /// is rewritten by `insteadOf`, duplicated, or spelled with other
    /// userinfo.
    SeveralUrls,
}

impl OriginFix {
    /// The fix for a repo whose `origin` needs the registry's URL.
    pub fn decide(config: &ConfigFacts) -> Self {
        let urls = &config.origin_urls;
        let by_hand = |reason| Self::ByHand { reason };
        if urls.iter().any(|v| !v.in_repo_file) {
            return by_hand(OriginByHand::OutsideRepoFile);
        }
        if urls.iter().any(|v| v.value.is_none()) {
            return by_hand(OriginByHand::ValuelessUrl);
        }
        if config.origin_keys != OriginKeys::InRepo {
            return Self::Add;
        }
        if urls.len() > 1 {
            return by_hand(if urls.iter().any(OriginUrl::resets) {
                OriginByHand::EmptyValue
            } else {
                OriginByHand::SeveralUrls
            });
        }
        // at most one value, maybe empty: `set-url` replaces it
        Self::SetUrl
    }
}

/// What `classify` derives for a present repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub branches: Vec<BranchStatus>,
    pub needs_human: Vec<NeedsHuman>,
    /// The repo's unprobed worktrees, each with what removing it would do.
    pub unprobed: Vec<UnprobedWorktreeStatus>,
}

/// What dropping an unprobed worktree's git dir would do — that worktree's
/// alone, as `git worktree remove <path>` drops it.
///
/// `None` unless it's `Prunable` (its dir gone); `Safe` when it loses
/// nothing; else what it loses — an operation's state, a HEAD that may be
/// the only ref to its commit (detached, unreadable, or on a branch that no
/// longer exists), what its git dir alone holds (submodules' repos,
/// per-worktree refs, staged changes — an index that can't be compared
/// counts, unless its HEAD is already lost; a git dir that can't be matched
/// counts as unknown), or, when a worktree git dir of
/// the repo names its worktree relatively, whatever git misreads (git
/// versions resolve a relative `gitdir` differently, so no path of the repo
/// is certain).
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
    if u.git_dir.is_none() {
        losses.push(PruneLoss::UnmatchedGitDir);
    }
    if let Some(holds) = &u.holds {
        if holds.submodules {
            losses.push(PruneLoss::Submodules);
        }
        if holds.worktree_refs {
            losses.push(PruneLoss::WorktreeRefs);
        }
        // not told: a lost HEAD already says the index can't be judged
        let head_lost = losses
            .iter()
            .any(|l| matches!(l, PruneLoss::UnknownHead | PruneLoss::MissingBranch { .. }));
        if holds.staged.unwrap_or(!head_lost) {
            losses.push(PruneLoss::StagedChanges);
        }
    }
    if let Some(git_dir) = &facts.relative_gitdir {
        losses.push(PruneLoss::RelativeGitdir {
            git_dir: git_dir.to_string_lossy().into_owned(),
        });
    }
    Some(if losses.is_empty() {
        Prune::Safe
    } else {
        Prune::Loses { losses }
    })
}

/// Classifies a present repo's facts against its registry entry, with the
/// live sessions in its checkouts.
///
/// Owned entries get a relation per branch. Third-party references are never
/// compared against a remote: they keep only branches with commits on no
/// remote, as `Untracked` — local work that can never be pushed.
pub fn classify(entry: &Entry, facts: &RepoFacts, sessions: &EntrySessions) -> Classified {
    let needs_human = needs_human(entry, facts, sessions);
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
            let on = checkouts_on(b, facts, sessions);
            let holds = Holds {
                entry: entry_held,
                on: &on,
                detection: sessions.detection,
            };
            let verdict = verdict(entry, b, relation, upstream.is_some(), &holds);
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
            busy: sessions.at(&u.path).to_vec(),
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
/// one of them busy, dirty, or unknown holds it.
// Independent facts folded over the checkouts, not a hidden state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CheckoutsOn<'a> {
    /// On HEAD in some checkout — or possibly, in one whose HEAD is unknown.
    checked_out: bool,
    /// A live session works in one of them.
    busy: bool,
    /// A live session may work in one of them, unseen: its path couldn't be
    /// resolved, or it's a worktree git names that the probe didn't find, so
    /// no session was scoped to it. Or a live session works through a git
    /// dir no worktree list names that may be on it (`UnlistedGitDir`).
    maybe_busy: bool,
    dirty: bool,
    unprobed: bool,
    /// The one checkout it's on, when that's a worktree `git worktree
    /// remove` would take: linked (never the main worktree), no submodules,
    /// not locked, no operation in progress, clean — and not a registry
    /// entry's dir, which is another entry's checkout to keep, nor one a
    /// live session works in, or might (busy detection unavailable, or its
    /// path unresolvable). An unprobed one is never removable here: a gone
    /// one's cleanup is its `prune`.
    removable: Option<&'a str>,
}

/// What may hold a branch's action back.
#[derive(Debug, Clone, Copy)]
struct Holds<'a, 'b> {
    /// An entry-level `needs_human` reason.
    entry: bool,
    on: &'b CheckoutsOn<'a>,
    detection: Detection,
}

impl Holds<'_, '_> {
    /// What holds `action`, if anything: an entry-level reason or a live
    /// session holds every action; a dirty checkout, or one that couldn't be
    /// probed, all but a push, which only moves refs; and a checkout that
    /// may be busy — busy detection unavailable, which leaves every checkout
    /// in doubt, or one on the branch whose path can't be resolved or that
    /// the probe didn't find, or an unlisted git dir a session works through
    /// — holds every action. The most specific reason
    /// names the hold.
    fn of(&self, action: SyncAction) -> Option<HeldBy> {
        let push = matches!(action, SyncAction::Push { .. });
        if self.entry {
            Some(HeldBy::Entry)
        } else if self.on.busy {
            Some(HeldBy::Busy)
        } else if !push && self.on.dirty {
            Some(HeldBy::DirtyCheckout)
        } else if !push && self.on.unprobed {
            Some(HeldBy::UnprobedWorktree)
        } else if self.on.maybe_busy || self.detection == Detection::Unavailable {
            Some(HeldBy::BusyUnknown)
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
/// root makes them differ. (Sessions, and checkouts that couldn't be
/// resolved, are looked up by the path each checkout's facts spell, which
/// is how `scope_sessions` keyed them — a session in an unprobed worktree's
/// files wherever they really are included, found by the `.git` it walks up
/// to.)
///
/// A branch git's `%(worktreepath)` says is checked out, but in no checkout
/// the probe found, may be busy: it's unprobed, and a session there was
/// scoped to nothing — the worktree list and `for-each-ref` race a worktree
/// being added. It holds every action, pushes included (`BusyUnknown`). A
/// bare repo's main worktree is the exception: git names it for its HEAD's
/// branch, but it has no files to hold. So does a git dir no worktree list
/// names that shares the refs, when a live session works through it and
/// its HEAD is on the branch or unknown.
fn checkouts_on<'a>(
    b: &BranchFacts,
    facts: &'a RepoFacts,
    sessions: &EntrySessions,
) -> CheckoutsOn<'a> {
    let name = b.branch.name.as_str();
    let on = |head: &Head| matches!(head, Head::Branch { name: n } if n == name);
    let busy = |path: &str| !sessions.at(path).is_empty();
    let maybe_busy = |path: &str| sessions.unresolved_at(path);
    let mut folded = CheckoutsOn::default();
    let mut count = 0;
    let mut removable = None;
    if on(&facts.status.head) {
        count += 1;
        folded.dirty |= !facts.status.uncommitted.is_clean();
        folded.busy |= busy(&facts.path);
        folded.maybe_busy |= maybe_busy(&facts.path);
    }
    for c in facts.worktrees.iter().filter(|c| on(&c.head)) {
        count += 1;
        folded.dirty |= !c.uncommitted.is_clean();
        folded.busy |= busy(&c.path);
        folded.maybe_busy |= maybe_busy(&c.path);
        let removable_here = c.linked
            && !facts.registry_worktrees.contains(&c.path)
            && c.submodules == Some(false)
            && !c.locked
            && c.in_progress.is_none()
            && c.uncommitted.is_clean()
            && !busy(&c.path)
            && !maybe_busy(&c.path)
            && sessions.detection == Detection::Available;
        if removable_here {
            removable = Some(c.path.as_str());
        }
    }
    let unprobed: Vec<&str> = facts
        .unprobed
        .iter()
        .filter(|u| match &u.head {
            UnprobedHead::Branch { name: n } => n == name,
            UnprobedHead::Detached { .. } => false,
            UnprobedHead::Unknown => true,
        })
        .map(|u| u.path.as_str())
        .collect();
    if !unprobed.is_empty() {
        count += unprobed.len();
        folded.unprobed = true;
        folded.busy |= unprobed.iter().any(|p| busy(p));
        folded.maybe_busy |= unprobed.iter().any(|p| maybe_busy(p));
    }
    // a git dir no worktree list names, sharing the refs, a session in it
    let unlisted = sessions.unlisted_on(name);
    if unlisted > 0 {
        count += unlisted;
        folded.maybe_busy = true;
    }
    // git says it's checked out, but in no checkout it listed: unknown, and
    // where it is no session was scoped to
    let elsewhere = b
        .branch
        .worktree
        .as_deref()
        .is_some_and(|w| Some(w) != facts.bare_main.as_deref());
    if count == 0 && elsewhere {
        folded.unprobed = true;
        folded.maybe_busy = true;
    }
    folded.checked_out = count > 0 || b.branch.worktree.is_some();
    if count == 1 {
        folded.removable = removable;
    }
    folded
}

/// What sync does with a branch. `has_upstream` is whether any upstream is
/// configured; `holds` what may hold its action back, including the
/// checkouts it's on.
fn verdict(
    entry: &Entry,
    b: &BranchFacts,
    relation: Relation,
    has_upstream: bool,
    holds: &Holds<'_, '_>,
) -> Verdict {
    let on = holds.on;
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
    holds
        .of(action)
        .map_or(Verdict::Act { action }, |by| Verdict::Held { action, by })
}

fn needs_human(entry: &Entry, facts: &RepoFacts, sessions: &EntrySessions) -> Vec<NeedsHuman> {
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
    let origin = match facts.config.origin_url() {
        Some(url) if origin_matches(url, &entry.url) => None,
        Some(url) => Some(OriginRemote::Url {
            url: without_userinfo(url).into_owned(),
        }),
        None if facts.config.origin_keys != OriginKeys::None => Some(OriginRemote::NoUrl),
        None => Some(OriginRemote::Missing),
    };
    if let Some(origin) = origin {
        reasons.push(NeedsHuman::OriginMismatch {
            origin,
            expected: entry.remote_url(),
            fix: OriginFix::decide(&facts.config),
        });
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
            // merge, cherry-pick, revert, sequencer, or am keeps HEAD on its
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
    // in the order the checkouts are probed: the primary, then the other
    // worktrees, probed or not. Said once: a checkout at or under a git dir
    // that can't be read (an unlisted worktree whose admin dir can't be
    // looked up is that admin dir) is that reason's to name, and it holds
    // the entry already
    let unreadable = |checkout: &str| {
        facts
            .unreadable
            .iter()
            .any(|p| std::path::Path::new(checkout).starts_with(p))
    };
    let checkouts = std::iter::once(&facts.path)
        .chain(facts.worktrees.iter().map(|c| &c.path))
        .chain(facts.unprobed.iter().map(|u| &u.path))
        .filter(|c| !unreadable(c));
    for checkout in checkouts {
        if let Some(u) = sessions.unresolved.get(checkout) {
            reasons.push(NeedsHuman::CheckoutUnresolvable {
                checkout: checkout.clone(),
                path: u.path.clone(),
                error: u.error.clone(),
            });
        }
    }
    for (git_dir, u) in &sessions.unlisted {
        reasons.push(NeedsHuman::UnlistedGitDir {
            git_dir: git_dir.clone(),
            head: u.head.clone(),
            busy: u.busy.clone(),
        });
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
        classify(entry, f, &EntrySessions::idle())
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
        classify(entry, f, &EntrySessions::idle())
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

    fn verdicts_with(entry: &Entry, f: &RepoFacts, sessions: &EntrySessions) -> Vec<Verdict> {
        classify(entry, f, sessions)
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
        let e = owned(follow("main"));
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
        let c = classify(&e, &f, &busy_at(&["/ws/app-lost"]));
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
        let e = owned(follow("main"));
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
                error: "checking /ws/sealed/app-locked/.git: Permission denied (os error 13)"
                    .into(),
            },
        )];
        let e = owned(follow("main"));
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
        let c = classify(&e, &f, &all);
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
                // (it may be on `old` too, so no worktree is removable)
                gone(None),
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
        let e = owned(follow("main"));
        let classified = classify(&e, &f, &busy_at(&["/ws/app-busy"]));
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
            verdicts(&owned(follow("main")), &f)[1..],
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
            verdicts(&owned(follow("main")), &f)[3],
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
        let c = classify(&owned(follow("main")), &f, &EntrySessions::idle());
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
        let c = classify(
            &third_party(CheckoutMode::Pinned),
            &f,
            &EntrySessions::idle(),
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
        let e = owned(follow("main"));
        let missing = facts(on("dev"), &[b("dev", O, true, Track::Even)]);
        assert_eq!(
            classify(&e, &missing, &EntrySessions::idle()).needs_human,
            [NeedsHuman::DefaultBranchMissing {
                branch: "main".into()
            }]
        );
        let no_upstream = facts(on("main"), &[b("main", None, false, Track::Even)]);
        assert_eq!(
            classify(&e, &no_upstream, &EntrySessions::idle()).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        let other_remote = facts(
            on("main"),
            &[b("main", Some("upstream"), true, Track::Even)],
        );
        assert_eq!(
            classify(&e, &other_remote, &EntrySessions::idle()).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        // unmapped is a branch-level reason, not a missing upstream
        let unmapped = facts(on("main"), &[b("main", O, false, Track::Even)]);
        assert!(
            classify(&e, &unmapped, &EntrySessions::idle())
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
            classify(&e, &detached, &EntrySessions::idle()).needs_human,
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
            classify(&e, &feature, &EntrySessions::idle())
                .needs_human
                .is_empty()
        );
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
        assert!(
            classify(&pinned, &detached, &EntrySessions::idle())
                .needs_human
                .is_empty()
        );
        assert_eq!(
            classify(&pinned, &on_main, &EntrySessions::idle()).needs_human,
            [NeedsHuman::PinnedOnBranch {
                branch: "main".into()
            }]
        );
        assert!(
            classify(&head, &detached, &EntrySessions::idle())
                .needs_human
                .is_empty()
        );
        assert!(
            classify(&head, &on_main, &EntrySessions::idle())
                .needs_human
                .is_empty()
        );
    }

    #[test]
    fn in_progress_and_origin_reasons() {
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        f.in_progress = Some(InProgressOp::Rebase);
        f.config.origin_urls.clear();
        f.config.origin_keys = OriginKeys::None;
        assert_eq!(
            classify(&owned(follow("main")), &f, &EntrySessions::idle()).needs_human,
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
            classify(&owned(follow("main")), &f, &EntrySessions::idle())
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
            classify(&owned(follow("main")), f, &EntrySessions::idle())
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
            classify(&owned(follow("main")), f, &EntrySessions::idle())
                .needs_human
                .into_iter()
                .find(|r| matches!(r, NeedsHuman::OriginMismatch { .. }))
        };
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
                classify(&owned(follow("main")), &f, &EntrySessions::idle()).needs_human,
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
                classify(&owned(follow("main")), &f, &EntrySessions::idle()).needs_human,
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
            classify(&owned(follow("main")), f, &EntrySessions::idle()).branches[0]
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
        let c = classify(&owned(follow("main")), &f, &EntrySessions::idle());
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
        assert!(
            classify(&e, &f, &EntrySessions::idle())
                .needs_human
                .is_empty()
        );

        // a rebase in a linked worktree doesn't explain the primary's detach
        f.status.head = Head::Detached {
            commit: "abc".into(),
        };
        f.worktrees[0].in_progress = Some(InProgressOp::Rebase);
        assert_eq!(
            classify(&e, &f, &EntrySessions::idle()).needs_human,
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
