//! The `repos status` report — the `--json` document and what the text
//! renderer reads.

use serde::Serialize;

use crate::STATUS_FORMAT_VERSION;
use crate::classify::NeedsHuman;
use crate::registry::{CheckoutMode, EntryKind, Visibility};
use crate::state::{BranchStatus, Checkout, Layout, Presence, UnprobedWorktreeStatus};

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// `STATUS_FORMAT_VERSION`.
    pub version: u32,
    /// The workspace root: the directory holding the registry as found.
    pub workspace: String,
    /// The registry's path as found.
    pub registry: String,
    pub entries: Vec<EntryStatus>,
    /// The workspace root's children holding a `.git` that no registry
    /// entry claims, by dir name; `None` when the scan didn't run (it runs
    /// only when no targets are given).
    pub unregistered: Option<Vec<UnregisteredClone>>,
}

impl StatusReport {
    pub const fn new(workspace: String, registry: String, entries: Vec<EntryStatus>) -> Self {
        Self {
            version: STATUS_FORMAT_VERSION,
            workspace,
            registry,
            entries,
            unregistered: None,
        }
    }
}

/// A child of the workspace root holding a `.git` that no registry entry
/// claims — a clone, or a worktree that isn't a live linked worktree of a
/// registered repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnregisteredClone {
    /// Its name under the workspace root (lossy when not UTF-8).
    pub dir: String,
    /// `remote.origin.url` as git reads it (includes applied; the first when
    /// several are set); `None` when it has none or git can't read its
    /// config.
    pub origin: Option<String>,
    /// Whether the origin's account is one of the registry's owners.
    pub owned: bool,
    /// Flattened: the stray's `kind` tag and its payload sit beside `dir`.
    #[serde(flatten)]
    pub kind: UnregisteredKind,
}

/// What an unregistered dir is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnregisteredKind {
    /// A repo of its own: a `.git` dir, or a `.git` file naming a git dir
    /// that isn't a linked worktree's.
    Clone,
    /// A worktree the scan offers no fix for, one of: a linked worktree of a
    /// repo the registry doesn't name; a worktree of a registered repo whose
    /// git dir sits outside its `worktrees/`, so git doesn't list it; a
    /// moved worktree whose `.git` is a link (replace the link with its
    /// file, then rerun `repos status`, which decides whether a repair is
    /// safe); or a `.git` that can't be read.
    Worktree,
    /// A linked worktree of a registered entry whose git dir names another
    /// path (or none) that doesn't use it: it was moved by hand. `git -C
    /// <entry dir> worktree repair <this path>` reconnects it — offered only
    /// when `blocked_by` is `None`. The repair also walks every other
    /// worktree git dir of the repo and rewrites the `.git` of each existing
    /// dir one names whose `.git` is missing or names another git dir;
    /// `blocked_by` is the first such dir, which a repair here would hijack.
    MovedWorktree {
        entry: String,
        blocked_by: Option<RepairBlock>,
        /// With the repair offered, a path git's walk over the other
        /// worktree git dirs will complain about, as the git dir writes it —
        /// one isn't a dir, or its `.git` isn't a file — exiting 1 while
        /// repairing this one all the same; `None` when the repair exits 0.
        exit_noise: Option<String>,
    },
    /// A linked worktree of a registered entry whose git dir is gone (pruned
    /// or deleted) or holds no `HEAD`: its index and HEAD are lost, and `git
    /// worktree repair` can't restore them.
    OrphanedWorktree { entry: String },
    /// A checkout whose `.git` names a git dir of a registered entry that
    /// the checkout at `with` uses, or may use: a copy of it, a copy of a
    /// locked worktree whose original is absent (unmounted media), one of
    /// several copies of a moved worktree, or an orphan whose git-dir id git
    /// reused for a newer worktree. `with` is `None` when git's record of
    /// that checkout can't be read, or is lost from a locked worktree's git
    /// dir. Git would show that checkout's index and HEAD here, and `git
    /// worktree repair` here would take the git dir from it — so no fix is
    /// offered.
    SharedGitDir { entry: String, with: Option<String> },
}

/// One registry entry's state.
// Independent declared facts, not a hidden state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
pub struct EntryStatus {
    pub key: String,
    pub kind: EntryKind,
    /// The dir name under the workspace root.
    pub dir: String,
    pub url: String,
    pub writable: bool,
    pub archived: bool,
    /// Declared on repos; references declare none.
    pub visibility: Option<Visibility>,
    /// Defaulted: a public repo runs CI unless it says otherwise.
    pub ci: bool,
    pub checkout_mode: CheckoutMode,
    pub presence: Presence,
    pub layout: Option<Layout>,
    /// The primary checkout first, then each of the repo's other worktrees
    /// probed.
    pub checkouts: Vec<Checkout>,
    pub branches: Vec<BranchStatus>,
    pub stashes: u32,
    /// `FETCH_HEAD`'s mtime, in unix seconds; `None` when never fetched.
    pub fetched_at: Option<u64>,
    pub needs_human: Vec<NeedsHuman>,
    /// A git call that failed after the repo was found; the facts above are
    /// then incomplete.
    // TODO: settle at the pass 1 checkpoint — a plain message for now, not
    // yet in the spec's types
    pub probe_error: Option<String>,
    /// The repo's worktrees that couldn't be probed — gone, or failing; the
    /// rest of the entry's facts stand.
    pub unprobed_worktrees: Vec<UnprobedWorktreeStatus>,
    /// Under `--fetch`, git's message when the fetch failed.
    // TODO: slice 2 classifies fetch failures from stderr (a missing remote
    // ref is the upstream gone; auth, host-key, and connection errors are the
    // host unreachable), likely turning this into a `kind`-tagged enum
    pub fetch_error: Option<String>,
}

/// What a repair of a moved worktree would also rewrite, so it isn't offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepairBlock {
    /// Another checkout, `path` (as the worktree git dir `git_dir` writes
    /// it), which `git_dir` names but whose `.git` is missing or names
    /// another git dir: fix it first.
    Rewrites { path: String, git_dir: String },
    /// This very dir: the worktree git dir `git_dir` names it, and a repair
    /// would point its `.git` there. The moved worktree whose `.git` names
    /// `git_dir` is to be repaired first, once its repair is offered — it's
    /// in the same list, and its own repair may be blocked too (a chain of
    /// moves settles one repair per run); then rerun.
    ClaimedDir { git_dir: String },
    /// This dir and `with`'s were swapped by hand: `git_dir` names this dir
    /// while `with`'s `.git` names it, and the other way round. Moving the
    /// two dirs back reconnects both; a repair of either would hijack the
    /// other.
    Swapped { git_dir: String, with: String },
    /// The repo's worktree git dir `git_dir` names its worktree by a relative
    /// path, which git 2.48+ resolves against the git dir and older gits
    /// against the cwd, so what a repair would touch is uncertain: fix it by
    /// hand. Decided before the other blocks, for any stray of the repo.
    RelativeGitdir { git_dir: String },
}
