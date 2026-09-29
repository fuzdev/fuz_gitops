//! A checkout's git state as the report carries it: facts, plus the
//! per-branch relation and verdict `classify` derives. No IO.

use serde::Serialize;

use crate::sessions::Session;

/// Whether an entry's dir holds a repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Presence {
    Present,
    Missing,
    NotARepo,
}

/// A checkout of the entry's repo: the primary — the registry's dir — or
/// another worktree of the same repo (a linked one, or the main worktree
/// when the registry's dir is itself linked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checkout {
    /// The primary's path is the workspace root joined with the entry's dir;
    /// another worktree's is the one git's worktree list prints.
    pub path: String,
    pub primary: bool,
    pub head: Head,
    pub uncommitted: Uncommitted,
    pub in_progress: Option<InProgressOp>,
    /// Locked with `git worktree lock`: git refuses to remove or prune it.
    pub locked: bool,
    /// A linked worktree, which `git worktree remove` can remove; `false`
    /// for the main worktree, which it refuses. The primary is linked when
    /// the registry's dir is itself a linked worktree.
    pub linked: bool,
    /// Whether `git worktree remove` would refuse it over submodules: one
    /// was initialized in it (its git dir holds `modules/`, even after
    /// `deinit`) or a gitlink in its index is populated. `None` when not
    /// checked: without `modules/`, the index is read only for a worktree
    /// that could otherwise be removed with its branch (linked, unlocked, no
    /// operation, clean, on a branch whose upstream is gone).
    pub submodules: Option<bool>,
    /// The live sessions working in it, by pid: each with a place (its
    /// recorded cwd, worktree, or process's cwd) that sits in it (deepest
    /// over every checkout probed), or whose `.git` — the one git finds
    /// walking up from that place — names its git dir, or in whose repo's
    /// `.claude/worktrees/` it is, where Claude Code would root theirs
    /// (subagent worktrees, whose sessions keep the parent's cwd; the
    /// `busy` module doc says where that is), or whose pid its lock names,
    /// a lock Claude Code wrote. Empty when none is, or when busy detection
    /// is unavailable, which the report's `sessions` says. A busy checkout
    /// holds every action on its branch, pushes included.
    pub busy: Vec<Session>,
}

/// A worktree that couldn't be probed as a checkout.
///
/// One `git worktree list` names that's gone or failing, or a git dir under
/// `<commondir>/worktrees/` the list leaves out. It's still a fact: an
/// operation in progress in it is a reason, and a branch checked out in it
/// is `HeldBy::UnprobedWorktree` — `HeldBy::Busy`, pushes included, when a
/// live session works in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnprobedWorktree {
    /// The worktree's path as git's worktree list prints it, or as its
    /// `gitdir` file names it; for one whose `gitdir` can't be read, its own
    /// git dir.
    pub path: String,
    /// Its own git dir, `<commondir>/worktrees/<id>`, canonicalized when it
    /// can be; `None` for one git lists that no git dir there matches.
    pub git_dir: Option<String>,
    pub head: UnprobedHead,
    pub locked: bool,
    /// From its own git dir, which outlives the worktree's files.
    pub in_progress: Option<InProgressOp>,
    pub why: UnprobedWhy,
    /// What its own git dir holds that may exist nowhere else; read only
    /// for a `Prunable` one, whose git dir `git worktree remove` would drop,
    /// and `None` for one whose git dir can't be matched (`git_dir` is
    /// `None`), which then counts as `PruneLoss::UnmatchedGitDir`.
    pub holds: Option<GitDirHolds>,
}

/// What a gone worktree's own git dir holds beyond its HEAD and operation
/// state — each gone with the git dir. Anything that can't be read counts
/// as held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GitDirHolds {
    /// `modules/` isn't empty: an initialized submodule's repo, whose
    /// commits may be nowhere else. Git refuses to remove a present worktree
    /// with submodules, but not a gone one.
    pub submodules: bool,
    /// `refs/` holds a ref: a per-worktree ref (`refs/worktree/`,
    /// `refs/bisect/`, `refs/rewritten/`), which may be the only ref to its
    /// commit.
    pub worktree_refs: bool,
    /// Whether its index differs from its HEAD — staged changes, in no
    /// commit (intent-to-add entries aside; no index at all is none);
    /// `None` when that couldn't be told (git failed, or its HEAD is
    /// unknown, so it wasn't asked).
    pub staged: Option<bool>,
}

/// An unprobed worktree as the report carries it: the probe's facts, plus
/// what `classify` decided about removing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnprobedWorktreeStatus {
    #[serde(flatten)]
    pub worktree: UnprobedWorktree,
    /// What dropping its git dir would lose — decided for this worktree
    /// alone, so the command that acts on it is `git worktree remove <path>`,
    /// which drops just this one (`git worktree prune` drops every gone
    /// worktree of the repo) — or, once the unregistered scan has run, that
    /// it moved to the workspace root; `Some` exactly when it's `Prunable`.
    pub prune: Option<Prune>,
    /// The live sessions working in it, as on a `Checkout`: they hold every
    /// action on its branch — on every branch, when its HEAD is unknown. A
    /// session in its files where they really are — moved by hand, copied,
    /// or on media mounted elsewhere — is here by the `.git` its place finds,
    /// which names this worktree's git dir, whatever `path` says; and a
    /// session a lock Claude Code wrote names, by its lock.
    pub busy: Vec<Session>,
}

/// What dropping one gone worktree's git dir would do, that worktree's
/// alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prune {
    /// Nothing is lost: its HEAD is on a branch that exists, no operation is
    /// in progress, and the repo's worktree paths read the same in every git.
    Safe,
    /// Dropping it discards these.
    Loses { losses: Vec<PruneLoss> },
    /// Not gone: moved into the workspace root, where the unregistered scan
    /// found each dir in `to` (by name) naming its git dir — so dropping
    /// the git dir would orphan them; their own lines say what to do.
    /// Decided after the scan, which runs only without targets, and sees
    /// only the root: without it, a moved worktree reads as `Safe` or
    /// `Loses`.
    Moved { to: Vec<String> },
}

/// Something dropping a gone worktree's git dir would discard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PruneLoss {
    /// Its operation's state (`rebase-merge/`, `MERGE_HEAD`, …).
    Operation { op: InProgressOp },
    /// A detached HEAD, which may be the only ref to its commit.
    DetachedHead,
    /// A HEAD that can't be read, so what it holds is unknown.
    UnknownHead,
    /// Its HEAD names a branch that no longer exists, so the HEAD may be the
    /// only ref to its commit.
    MissingBranch { name: String },
    /// Its git dir holds an initialized submodule's repo (`modules/`),
    /// whose commits may be nowhere else.
    Submodules,
    /// Its git dir holds a per-worktree ref, which may be the only ref to
    /// its commit.
    WorktreeRefs,
    /// Its index differs from its HEAD, or couldn't be compared: staged
    /// changes that are in no commit.
    StagedChanges,
    /// Git lists it, but no worktree git dir matches it, so nothing its git
    /// dir holds can be read: whatever that is.
    UnmatchedGitDir,
    /// The repo's worktree git dir `git_dir` names its worktree by a
    /// relative path, which git 2.48+ resolves against the git dir and older
    /// gits against the cwd: this worktree may not be gone at all, or a
    /// removal by its path may reach another — its index and HEAD are at
    /// stake.
    RelativeGitdir { git_dir: String },
}

/// What an unprobed worktree's HEAD is, from git's worktree list or, for one
/// it doesn't list, the worktree's own `HEAD` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnprobedHead {
    Branch {
        name: String,
    },
    Detached {
        commit: String,
    },
    /// Unreadable: any branch might be checked out there, so every
    /// fast-forward or move in the entry is held.
    Unknown,
}

/// Why a worktree wasn't probed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnprobedWhy {
    /// Its dir is gone and git would prune it — or it was moved by hand.
    /// Moved back, it reconnects; `git worktree repair` at the new path is
    /// repo-wide and may hijack another checkout, so it's offered only by the
    /// unregistered scan, which vets it for a stray at the workspace root.
    Prunable,
    /// Its dir is gone but git keeps it — locked, as on unmounted media.
    Missing,
    /// It's there but couldn't be probed — no `.git`, unreadable, a failed
    /// status — or git doesn't list it.
    Failed { error: String },
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
    /// Either backend: `rebase-merge/`, or `rebase-apply/` without am's
    /// `applying` mark.
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
    /// `git am`: `rebase-apply/` marked `applying`. Unlike a rebase, it
    /// applies onto the branch HEAD is on.
    Am,
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
            Self::Am => "am",
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
    /// The full ref it points at when it's a symbolic ref (`refs/heads/m` →
    /// `refs/heads/main`): an alias, always `Quiet` with no commits counted.
    /// What it points at holds the commits — a local branch, reported with
    /// its own verdict; a remote-tracking ref, whose commits a remote has —
    /// and a write through the alias would move that target past every
    /// check made on the alias.
    pub symref: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Deletable by hand; sync never deletes. `removable_worktree` is the
    /// clean linked worktree it's checked out in, removable along with it;
    /// `None` when it's in none, the one it's in is dirty (and its dirt
    /// shows as uncommitted), is a registry entry's dir, or is busy (a live
    /// session works in it) — or when busy detection is unavailable, so any
    /// checkout may be.
    Cleanup {
        reason: CleanupReason,
        removable_worktree: Option<String>,
    },
}

/// What `sync` does with an entry whose dir is missing: clone it, by
/// `recipe` — decided in `classify`, as a branch's verdict is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CloneVerdict {
    /// Sync clones it.
    Act { recipe: CloneRecipe },
    /// Sync would clone it, but `by` holds it: `Entry`, another entry
    /// naming the same repo (the entry's `clone_shares_repo` reason);
    /// `Busy`, a live session working at the missing path (its dir deleted
    /// from under it); or `UnprobedWorktree`, another entry's gone worktree
    /// recorded there.
    /// Busy detection that's unavailable holds no clone: a missing dir
    /// holds no work to lose, and the clone never replaces anything.
    Held { recipe: CloneRecipe, by: HeldBy },
}

impl CloneVerdict {
    pub const fn recipe(&self) -> &CloneRecipe {
        match self {
            Self::Act { recipe } | Self::Held { recipe, .. } => recipe,
        }
    }
}

/// How `sync` clones a missing entry, from its registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloneRecipe {
    /// Where the clone comes from, and what `origin` holds after:
    /// `Entry::remote_url` — SSH for an owned entry, HTTPS for a
    /// third-party one, the only transport the clone may use.
    pub url: String,
    /// The branch the clone checks out (`--branch`), its upstream origin's
    /// branch of the name; `None` takes the remote's default branch.
    pub branch: Option<String>,
    /// `--depth 1`, which maps only the cloned branch.
    pub shallow: bool,
    /// The one subtree checked out, in cone mode, cloned
    /// `--filter=blob:none`.
    pub sparse: Option<String>,
}

/// What holds a branch's action back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldBy {
    /// The entry is pinned: its consumer moves HEAD, never the tool, so
    /// every fast-forward and move in it is held for good, whether HEAD is
    /// detached or on a branch — a stale local branch beside the pin
    /// included, since a pin is never fetched. Named before any other hold:
    /// clearing those never releases a pin. (A pin's pushes aren't held: the
    /// tool leaves a pin alone, pushes included, so a branch ahead reads
    /// `LocalOnly` when it has commits on no remote ref, else `Quiet`.)
    Pinned,
    /// An entry-level `needs_human` reason stops sync on the whole entry —
    /// a missing entry's clone included (`clone_shares_repo`).
    Entry,
    /// A push through `origin` would reach somewhere other than the
    /// registry's repo over SSH (the entry's `push_url_mismatch` reason):
    /// pushes only.
    PushUrl,
    /// The entry's fetch failed or was refused (its `fetch_error`), so its
    /// remote-tracking refs weren't refreshed and may not be origin's: no
    /// branch fast-forwards or moves to them, and none is pushed — its
    /// ahead count is unverified, a push's own remote-tracking update goes
    /// through the refspecs a refusal found unconfinable, and a host that
    /// just failed a fetch fails the push too. Only a run that fetches
    /// (`sync`, `status --fetch`) holds on it.
    FetchFailed,
    /// The branch is checked out in a checkout with uncommitted changes, and
    /// sync never touches a dirty working tree. Pushes aren't held: they only
    /// move refs.
    DirtyCheckout,
    /// The branch is checked out in a worktree that couldn't be probed (one
    /// of the entry's `unprobed_worktrees`), so whether it's clean is
    /// unknown. Pushes aren't held. A clone is held when its missing path is
    /// one of another entry's unprobed worktrees: git still records a
    /// worktree there, which would take the clone for its own files —
    /// remove that record first.
    UnprobedWorktree,
    /// The branch is checked out in more than one checkout (`worktree add
    /// -f`): moving it in one would leave the others' HEAD on a commit their
    /// files don't match. Pushes aren't held.
    SeveralCheckouts,
    /// The branch is checked out in a checkout a live session works in
    /// (its `busy`): sync leaves another session's branch alone, pushes
    /// included. A clone is held when a live session works at or under
    /// its missing path — its dir deleted from under it — where the clone
    /// would land in its place.
    Busy,
    /// A live session may work in a checkout, unseen, so every action is
    /// held, pushes included: busy detection is unavailable (the report's
    /// `sessions` says why), so any checkout may be busy; or the branch is
    /// checked out in a checkout whose path can't be resolved (a
    /// `checkout_unresolvable` reason); or git says it's checked out in a
    /// worktree its worktree list doesn't name (a race with a worktree added
    /// mid-probe), so no session was scoped there; or a live session works
    /// through a git dir sharing the repo's refs that no worktree list names
    /// (an `unlisted_git_dir` reason), whose HEAD is on it or unknown.
    BusyUnknown,
    /// A push, with an agent running the tool (`Caller::Agent`): an agent's
    /// pushes wait for the gateway, and a person runs sync to push them.
    /// Named only when nothing else holds the push, so it says the person's
    /// sync would push it.
    Gateway,
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
    /// Ahead, but its upstream's ref on origin isn't a branch a push can
    /// name: outside `refs/heads/`, or `refs/heads/HEAD` (an upstream set to
    /// `origin/HEAD` would create a branch named `HEAD` on the remote).
    UpstreamNotABranch,
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
