//! A checkout's git state as the report carries it: facts, plus the
//! per-branch relation and verdict `classify` derives. No IO.

use serde::Serialize;

/// Whether an entry's dir holds a repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Presence {
    Present,
    Missing,
    NotARepo,
}

/// A checkout: the main one, or (from pass 2) a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checkout {
    pub path: String,
    pub primary: bool,
    pub head: Head,
    pub uncommitted: Uncommitted,
    pub in_progress: Option<InProgressOp>,
}

/// What a checkout's HEAD points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Head {
    Branch { name: String },
    Detached { commit: String },
}

/// Uncommitted changes in a checkout, split by kind. A path both staged and
/// modified since counts once on each side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Uncommitted {
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl Uncommitted {
    pub const fn total(&self) -> u32 {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }

    pub const fn is_clean(&self) -> bool {
        self.total() == 0
    }
}

/// An operation stopped mid-way in a checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InProgressOp {
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
}

impl InProgressOp {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Bisect => "bisect",
            Self::Sequencer => "sequencer",
        }
    }
}

/// One local branch and its relation to its remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchStatus {
    pub name: String,
    /// As configured, e.g. `origin/main` or `upstream/main`.
    pub upstream: Option<String>,
    /// The checkout it's checked out in.
    pub worktree: Option<String>,
    /// Commits on no remote-tracking ref (`rev-list <b> --not --remotes`),
    /// minus shallow roots. Counted only where the relation leaves room for
    /// local work; zero otherwise.
    pub unique_commits: u32,
    /// The newest commit's committer time, in unix seconds.
    pub newest_commit_at: u64,
    pub relation: Relation,
    /// What `sync` does with the branch — the one decision `status` previews
    /// and `sync` executes.
    pub verdict: Verdict,
}

/// What `sync` does with a branch, given its relation, its entry, and the
/// entry's `needs_human` reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// Nothing to do or to say.
    Quiet,
    /// Sync takes the action.
    Act { action: SyncAction },
    /// Sync would take the action, but something holds it back until a
    /// person clears it.
    Held { action: SyncAction, by: HeldBy },
    /// Sync won't touch the branch; a person decides.
    NeedsHuman { reason: BranchNeedsHuman },
    /// Commits on no remote that sync never pushes: no origin upstream, a
    /// read-only entry, or a pinned one.
    LocalOnly,
    /// Deletable by hand; sync never deletes.
    Cleanup { reason: CleanupReason },
}

/// What holds a branch's action back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldBy {
    /// An entry-level `needs_human` reason stops sync on the whole entry.
    Entry,
    /// The branch is checked out in a checkout with uncommitted changes, and
    /// sync never touches a dirty working tree. Pushes aren't held: they only
    /// move refs.
    DirtyCheckout,
    /// The branch is checked out in a linked worktree whose state isn't
    /// probed, so whether it's clean is unknown. Pushes aren't held.
    // TODO: gone once pass 2 probes linked worktrees
    UnprobedWorktree,
}

/// A move `sync` makes on a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncAction {
    Push {
        commits: u32,
    },
    FastForward {
        commits: u32,
    },
    /// A shallow branch with nothing local, moved to the fetched tip.
    Move,
}

/// Why sync leaves a branch to a person. The counts are on its relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchNeedsHuman {
    /// Placing it needs a force-push or a rebase, which sync never does.
    Diverged,
    /// Its origin upstream lies outside the fetch refspec.
    Unmapped,
    /// Ahead on an archived repo, whose host refuses writes.
    ArchivedAhead,
    /// A shallow branch with local commits off the fetched tip.
    ShallowLocalWork,
}

/// Why a branch reads as deletable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupReason {
    /// No unique commits and no upstream: its work is on a remote.
    Merged,
    /// Its upstream was deleted — likely merged, but a squash merge leaves
    /// its commits reading unique.
    UpstreamGone,
}

/// A local branch's relation to its remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Relation {
    InSync,
    Ahead {
        commits: u32,
    },
    Behind {
        commits: u32,
    },
    Diverged {
        ahead: u32,
        behind: u32,
    },
    /// A shallow clone whose tips differ and can't be compared; commits on
    /// the fetched tip are `Ahead` instead. With no unique commits nothing
    /// local is at stake, so the branch can move to the fetched tip; with
    /// some, it needs a human.
    Shallow,
    /// An origin upstream is configured but its tracking ref was pruned.
    Gone,
    /// The origin upstream lies outside the fetch refspec, so git resolves
    /// none.
    Unmapped,
    /// No origin upstream: none at all, or another remote's.
    Untracked,
}

/// How a checkout is laid out on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layout {
    pub shallow: bool,
    pub sparse: bool,
    pub partial_filter: Option<String>,
}
