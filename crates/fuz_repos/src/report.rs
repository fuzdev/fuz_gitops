//! The `repos status` report — the `--json` document and what the text
//! renderer reads.

use serde::Serialize;

use crate::STATUS_FORMAT_VERSION;
use crate::busy::Sessions;
use crate::classify::NeedsHuman;
use crate::error::{Error, ErrorKind};
use crate::registry::{EntryKind, Visibility};
use crate::remote::{RemoteFailure, VisibilityCheck};
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
    /// Whether the run was asked to fetch (`--fetch`) — not whether any
    /// fetch succeeded, or ran at all: true even when every fetch failed, or
    /// no entry was one `--fetch` fetches. Each entry's `fetch_error` says
    /// how its own fetch went; entries `--fetch` passes over (third-party,
    /// pinned, or with no `origin` URL) weren't fetched either way. The
    /// visibility check ran exactly when this is true.
    pub fetched: bool,
    /// Busy detection: the live Claude Code sessions in no checkout, or why
    /// they couldn't be vouched for (every push, fast-forward, and move is
    /// then held). Those in a checkout are on it, as its `busy`.
    pub sessions: Sessions,
    pub entries: Vec<EntryStatus>,
    /// The workspace root's children holding a `.git` that no registry
    /// entry claims, by dir name; `None` when the scan didn't run (it runs
    /// only when no targets are given).
    pub unregistered: Option<Vec<UnregisteredClone>>,
}

impl StatusReport {
    pub const fn new(
        workspace: String,
        registry: String,
        fetched: bool,
        sessions: Sessions,
        entries: Vec<EntryStatus>,
    ) -> Self {
        Self {
            version: STATUS_FORMAT_VERSION,
            workspace,
            registry,
            fetched,
            sessions,
            entries,
            unregistered: None,
        }
    }
}

/// The `--json` document for a fatal error, printed on stdout in place of
/// the report.
///
/// A consumer never parses empty stdout. The document carries the same
/// `version` as the report; a consumer tells the two apart by `error`.
/// Argument-parse errors precede knowing `--json` and stay plain text on
/// stderr.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorReport {
    /// `STATUS_FORMAT_VERSION`.
    pub version: u32,
    pub error: ErrorBody,
}

/// A fatal error: its kind (flattened: the `kind` tag and its payload sit
/// beside `message`), the message the binary prints after `error: `, and the
/// hint it prints after `hint: `.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    #[serde(flatten)]
    pub kind: ErrorKind,
    pub message: String,
    pub hint: Option<String>,
}

impl ErrorReport {
    pub fn new(e: &Error) -> Self {
        Self {
            version: STATUS_FORMAT_VERSION,
            error: ErrorBody {
                kind: e.kind(),
                message: e.message(),
                hint: e.hint().map(std::borrow::Cow::into_owned),
            },
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
    /// several are set, after any empty value resetting the list), with a
    /// credential in its userinfo redacted as `***`; `None` when it has none
    /// or git can't read its config.
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
    /// The branch the checkout lives on, as on the registry's `Entry`: a
    /// repo's default branch, a reference's declared one, else `None`.
    pub branch: Option<String>,
    /// Its consumer moves HEAD, never the tool: never fetched, and every
    /// fast-forward and move in it `HeldBy::Pinned`.
    pub pinned: bool,
    pub presence: Presence,
    /// `None` when the repo isn't present, or its probe failed before the
    /// config was read; with `probe_error` set it's what was read first.
    pub layout: Option<Layout>,
    /// The primary checkout first, then each of the repo's other worktrees
    /// probed.
    pub checkouts: Vec<Checkout>,
    pub branches: Vec<BranchStatus>,
    pub stashes: u32,
    /// The newest non-empty `FETCH_HEAD`'s mtime across the repo's
    /// worktrees, in unix seconds; `None` when never fetched — or when the
    /// last fetch failed (git empties `FETCH_HEAD` then, so the remote view's
    /// age is unknown) or found an empty remote.
    pub fetched_at: Option<u64>,
    pub needs_human: Vec<NeedsHuman>,
    /// A git call that failed after the repo was found; the facts above are
    /// then incomplete. On a partial clone the call may have needed an
    /// object the clone lacks (`probe_failed_partial`).
    // TODO: settle at the pass 1 checkpoint — a plain message for now, not
    // yet in the spec's types
    pub probe_error: Option<String>,
    /// The repo's worktrees that couldn't be probed — gone, or failing; the
    /// rest of the entry's facts stand.
    pub unprobed_worktrees: Vec<UnprobedWorktreeStatus>,
    /// Why the fetch failed or was refused, under `--fetch`: `Some` for a
    /// fetch git ran and failed, and for one the tool refused to run
    /// (`refspec_outside_origin`, `origin_refs_shared`,
    /// `legacy_remotes_unreadable`). `None` when it
    /// succeeded or wasn't attempted — `--fetch` not given, an entry it
    /// passes over, or one whose `origin` has no URL, which origin drift
    /// reports instead. The rest of the entry is probed either way, from the
    /// remote-tracking refs as they stand.
    pub fetch_error: Option<RemoteFailure>,
    /// What an anonymous read of the repo found, under `--fetch`, for a
    /// `[repos]` entry declared private; `None` when the check didn't run
    /// (no `--fetch`, or not declared private).
    pub visibility_check: Option<VisibilityCheck>,
}

impl EntryStatus {
    /// Whether the probe failed on a partial clone (its layout, read before
    /// any call that needs objects, carries a filter): a call may have
    /// needed an object the clone lacks, and the probe never fetches one on
    /// demand. Keyed on the filter, never git's message — a `checkout` in
    /// the clone fetches what's missing from origin and fills the checkout.
    pub fn probe_failed_partial(&self) -> bool {
        self.probe_error.is_some()
            && self
                .layout
                .as_ref()
                .is_some_and(|l| l.partial_filter.is_some())
    }
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
