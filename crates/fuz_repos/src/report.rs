//! The `--json` documents — `repos status`'s report, `repos sync`'s and
//! `repos push`'s, and a fatal error's — and what the text renderer reads.

use std::path::Path;

use serde::Serialize;

use crate::classify::NeedsHuman;
use crate::error::{Error, ErrorKind};
use crate::paths::same_path;
use crate::registry::{EntryKind, Visibility};
use crate::remote::{RemoteFailure, VisibilityCheck};
use crate::sessions::{Session, Unavailable};
use crate::state::{
    AtRest, BranchNeedsHuman, BranchStatus, Checkout, CloneVerdict, HeldBy, Layout, Presence,
    RefreshVerdict, SyncAction, UnprobedWorktreeStatus,
};
use crate::{PUSH_FORMAT_VERSION, STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};

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
    /// how its own fetch went; entries `--fetch` passes over (pinned, a
    /// third-party reference the run doesn't refresh, or with no `origin`
    /// URL) weren't fetched either way. The visibility check ran exactly
    /// when this is true.
    pub fetched: bool,
    /// Busy detection: the live Claude Code sessions in no checkout, or why
    /// they couldn't be vouched for (every push, fast-forward, and move is
    /// then held). Those in a checkout are on it, as its `busy`.
    pub sessions: Sessions,
    pub entries: Vec<EntryStatus>,
    /// The workspace root's children holding a `.git` that no registry
    /// entry claims, by dir name; `None` when the scan didn't run (it runs
    /// only when no targets are given). A missing entry whose repo one of
    /// them clones is held (`cloned_unregistered`) only when it ran.
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

/// Busy detection as the report carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sessions {
    /// Every live session was vouched for. Those in a checkout are on it
    /// (`busy`): by path, by the git dir git would find from a place of
    /// theirs, as the parent of a worktree under the `.claude/worktrees/`
    /// Claude Code would root theirs at, or by the lock Claude Code put on
    /// it for them;
    /// those working through a git dir no worktree list names that shares an
    /// entry's refs are on that entry's `unlisted_git_dir` reason.
    /// `unscoped` are the rest, by pid and cwd — at the workspace root,
    /// outside it, or in no checkout the run probed (with targets, other
    /// entries' included). They never block.
    Available { unscoped: Vec<Session> },
    /// Some live session couldn't be vouched for: every push, fast-forward,
    /// and move is held, as though every checkout were busy.
    Unavailable { reason: Unavailable },
}

/// The `--json` document for a fatal error, printed on stdout in place of
/// the report.
///
/// A consumer never parses empty stdout. The document carries the same
/// `version` as the command's report (`STATUS_FORMAT_VERSION`,
/// `SYNC_FORMAT_VERSION`, or `PUSH_FORMAT_VERSION`); a consumer tells the
/// two apart by `error`.
/// Argument-parse errors precede knowing `--json` and stay plain text on
/// stderr.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorReport {
    /// The command's report version.
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
    /// `e` as the document of a command whose report is at `version`.
    pub fn new(e: &Error, version: u32) -> Self {
        Self {
            version,
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
    /// `blocked_by` is the first such dir, which a repair here would hijack,
    /// or what else keeps the repair from being offered (`RepairBlock`).
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
    /// A clone's temp dir, named `.<dir>.repos-clone-<pid>-<nonce>`
    /// (`clone::is_temp_dir_name`), with a `.git` or without one: a clone a
    /// sync didn't finish or clean up — killed mid-clone, or unable to
    /// remove it — or one a sync is still running, which the scan can't
    /// tell apart. The tool's own, never a repo to keep: remove it once no
    /// `repos sync` is running.
    UnfinishedClone,
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
    /// What the run does about refreshing it, when it was asked to — named
    /// as a target, or under `--references` (`refresh_verdict`): a
    /// third-party reference refreshed (fetched, and its branches compared
    /// against origin as an owned repo's are), or a pin refused, or one
    /// whose origin isn't the registry's repo held (`HeldBy::Entry`), or
    /// whose fetch wouldn't reach it over HTTPS (`HeldBy::OriginNotHttps`).
    /// `None` when the run didn't ask, for an owned entry that isn't pinned
    /// (synced either way), and when no repo is at the entry's dir.
    pub refresh: Option<RefreshVerdict>,
    pub presence: Presence,
    /// What sync does about a missing dir — clone it, or why not yet
    /// (`classify_missing`); `Some` exactly when `presence` is missing.
    pub clone: Option<CloneVerdict>,
    /// `None` when the repo isn't present, or its probe failed before the
    /// config was read; with `probe_error` set it's what was read first.
    pub layout: Option<Layout>,
    /// The primary checkout first, then each of the repo's other worktrees
    /// probed.
    pub checkouts: Vec<Checkout>,
    pub branches: Vec<BranchStatus>,
    /// Whether the primary checkout is at rest where the registry puts it:
    /// on the followed branch, clean, idle, and that branch's relation
    /// (`classify::at_rest`). `None` exactly when `checkouts` is empty —
    /// the repo is missing or isn't one, or its probe failed
    /// (`probe_error`), so the primary's state wasn't read whole.
    pub at_rest: Option<AtRest>,
    pub stashes: u32,
    /// The newest non-empty `FETCH_HEAD`'s mtime across the repo's
    /// worktrees, in unix seconds — or, when none of them has a `FETCH_HEAD`
    /// at all, the time of `git clone`'s own reflog entry (a clone writes no
    /// `FETCH_HEAD`; the time is the entry's ident date). `None` when never
    /// fetched and no clone entry is left (a repo made by `git init` or
    /// cloned empty, its refs in the reftable format, or its reflog expired)
    /// — or when the last fetch failed (git empties
    /// `FETCH_HEAD` then, so the remote view's age is unknown) or found an
    /// empty remote.
    pub fetched_at: Option<u64>,
    pub needs_human: Vec<NeedsHuman>,
    /// A git call that failed after the repo was found, as a plain message;
    /// the facts above are then incomplete. On a partial clone the call may
    /// have needed an object the clone lacks (`probe_failed_partial`).
    pub probe_error: Option<String>,
    /// The repo's worktrees that couldn't be probed — gone, or failing; the
    /// rest of the entry's facts stand.
    pub unprobed_worktrees: Vec<UnprobedWorktreeStatus>,
    /// Why the fetch failed or was refused, under `--fetch`: `Some` for a
    /// fetch git ran and failed, and for one the tool refused to run
    /// (`refspec_outside_origin`, `origin_refs_shared`,
    /// `legacy_remotes_unreadable`). `None` when it
    /// succeeded or wasn't attempted — `--fetch` not given, an entry it
    /// passes over, or one whose `origin` isn't the registry's repo or has
    /// no URL, which origin drift reports instead. The rest of the entry is
    /// probed either way, from the remote-tracking refs as they stand.
    pub fetch_error: Option<RemoteFailure>,
    /// What an anonymous read of the repo found, under `--fetch`, for a
    /// `[repos]` entry declared private; `None` when the check didn't run
    /// (no `--fetch`, or not declared private).
    pub visibility_check: Option<VisibilityCheck>,
}

impl EntryStatus {
    /// Whether `branch` is the branch the entry follows, its upstream gone
    /// from origin (its `default_branch_gone` reason, classify's): never
    /// recreated on origin, and never cleanup.
    pub fn default_branch_gone(&self, branch: &str) -> bool {
        self.needs_human
            .iter()
            .any(|r| matches!(r, NeedsHuman::DefaultBranchGone { branch: b } if b == branch))
    }

    /// The probed checkout at `path`, compared as the kernel resolves both
    /// (`paths::same_path`); `None` when none of them is.
    pub fn checkout_at(&self, path: &Path) -> Option<&Checkout> {
        self.checkouts
            .iter()
            .find(|c| same_path(Path::new(&c.path), path))
    }

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
    /// The repo's worktree git dir `git_dir` has a `gitdir` that's there but
    /// can't be read (or the git dir itself can't be resolved), so what a
    /// repair's walk over it would rewrite is unknown: fix it by hand.
    /// Decided after `RelativeGitdir`, before the rest, for any stray of the
    /// repo.
    UnreadableGitdir { git_dir: String },
    /// This dir's path isn't UTF-8, so the report can't name it exactly in
    /// a command: rename it to a UTF-8 name, then rerun. Decided after the
    /// blocks above, before `NulInGitdir`, whose fix names this dir.
    NonUtf8Path,
    /// This dir's own worktree git dir, `git_dir`, holds a NUL in its
    /// `gitdir`: git lists the worktree by what's before the NUL, while a
    /// repair of this dir compares that with this dir's `.git` and may find
    /// nothing to fix. Rewrite that `gitdir` by hand as this dir's `.git`
    /// path (what a repair would write), then rerun.
    NulInGitdir { git_dir: String },
}

/// The `repos sync --json` document: the state sync acted on, and what it
/// did.
#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    /// `SYNC_FORMAT_VERSION`.
    pub version: u32,
    /// What sync acted on: a `status --fetch` report of the state its fetch
    /// left (so `fetched` is true), each branch's verdict classified with
    /// the live sessions read after the fetch. The unregistered scan runs
    /// before the fetch, as `status`'s does, and is reported only without
    /// targets (`unregistered` is `null` with them). Parsed with the status report's
    /// own schema: its `version` is `STATUS_FORMAT_VERSION`.
    pub status: StatusReport,
    /// What sync did, one per `status` entry, in its order.
    pub entries: Vec<EntrySync>,
}

impl SyncReport {
    pub const fn new(status: StatusReport, entries: Vec<EntrySync>) -> Self {
        Self {
            version: SYNC_FORMAT_VERSION,
            status,
            entries,
        }
    }

    /// Whether anything failed — a fetch (one git ran and failed, or one
    /// the tool refused to run, its refspec unconfinable: either way the
    /// entry wasn't synced and a person must act), a probe, or an action (a
    /// push the remote refused or couldn't be reached for included) — so
    /// the run exits `1`. A hold, a person's call, or busy detection that
    /// couldn't vouch for every session is the report's to say, not a
    /// failure.
    pub fn failed(&self) -> bool {
        self.entries.iter().any(|e| {
            matches!(e.fetch, FetchOutcome::Failed { .. })
                || e.clone.as_ref().is_some_and(CloneOutcome::failed)
                || e.branches.iter().any(|b| b.outcome.failed())
        }) || self.status.entries.iter().any(|e| e.probe_error.is_some())
    }
}

/// What sync did with one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntrySync {
    pub key: String,
    pub fetch: FetchOutcome,
    /// What sync did about a missing dir: `Some` exactly when the status
    /// entry has a `clone` verdict.
    pub clone: Option<CloneOutcome>,
    /// One per branch of the status entry, in its order. Empty when there
    /// are none: the repo is missing (a clone's branch is in its `clone`
    /// outcome), isn't one, or its probe failed — sync refuses an entry it
    /// couldn't read whole, and that counts as failed (the status entry's
    /// `probe_error`).
    pub branches: Vec<BranchSync>,
}

/// What sync did about a missing entry: its `clone` verdict, carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CloneOutcome {
    /// Cloned into place and read back there: HEAD on `branch` (the
    /// recipe's, else the remote's default) at `head`, its upstream
    /// origin's branch of the name, the checkout clean.
    Cloned { branch: String, head: String },
    /// Not cloned: the verdict's hold, or one found right before cloning —
    /// `busy`, a live session now at or under the path, or `changed`,
    /// something now at the path.
    Held { by: SyncHold },
    /// Git's clone (or its sparse checkout) failed reaching for the remote
    /// or at it, classified as a fetch failure is; nothing is left at the
    /// path. The run exits `1`.
    CloneFailed { failure: RemoteFailure },
    /// The clone couldn't be made or placed (`message` says why; nothing is
    /// left at the path), or read back in place as the recipe says it
    /// should be — the clone stays there, and `repos status` shows it.
    /// The run exits `1`.
    Failed { message: String },
}

impl CloneOutcome {
    /// Whether the clone failed, so the run exits `1`.
    const fn failed(&self) -> bool {
        matches!(self, Self::CloneFailed { .. } | Self::Failed { .. })
    }
}

/// How sync's fetch of an entry went.
///
/// A third-party reference's refresh (its status entry's `refresh`
/// verdict) has no outcome of its own: it's this fetch, then its branches'
/// outcomes — `fetched`, and each branch fast-forwarded, moved, untouched,
/// or held, is the reference refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FetchOutcome {
    Fetched,
    /// Git failed, or the tool refused to run it (`status`'s
    /// `fetch_error`, the same value).
    Failed {
        failure: RemoteFailure,
    },
    /// Not attempted: an entry sync doesn't fetch (a third-party reference
    /// the run doesn't refresh, a pin), a repo that's missing (cloned
    /// instead) or isn't one, one whose `origin` isn't the registry's repo
    /// or has no URL (its `origin_mismatch` reason holds it), or a probe
    /// that failed before its fetch.
    NotFetched,
}

/// What sync did with one branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchSync {
    pub name: String,
    /// Flattened: the outcome's `kind` tag and its payload sit beside
    /// `name`.
    #[serde(flatten)]
    pub outcome: BranchOutcome,
    /// The key of the entry sharing this one's repo whose outcome this is:
    /// a branch acts once for the repo, so an entry after the first to act
    /// on it reports that one's outcome, and one whose action another entry
    /// stopped (by holding the branch, or leaving it to a person) reports
    /// that entry's. `None` for the entry's own outcome. The text summary
    /// counts each outcome once, under the entry it names.
    pub repeats: Option<String>,
}

/// What sync did with a branch: its verdict, carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchOutcome {
    /// Nothing for sync to do: the verdict was quiet, local-only work, or
    /// cleanup (which sync never does) — or the branch was already where the
    /// action would have put it when sync came to it.
    Untouched,
    /// The verdict left it to a person.
    NeedsHuman { reason: BranchNeedsHuman },
    /// Sync would take `action`, but `by` held it — the verdict's hold, or
    /// one found when sync came to act.
    Held { action: SyncAction, by: SyncHold },
    /// Fast-forwarded from `from` to `to`, in place or in its clean
    /// checkout.
    FastForwarded { from: String, to: String },
    /// A shallow branch with nothing local moved from `from` to the fetched
    /// tip `to`.
    Moved { from: String, to: String },
    /// Pushed to the branch's upstream on the registry's repo: the remote's
    /// branch moved from `from`, the fetched tip — the lease held it there —
    /// to `to`, the commit classified: a fast-forward. A push never creates
    /// a branch.
    Pushed { from: String, to: String },
    /// The push reached for the remote and failed there: refused
    /// (`rejected`, a ruleset or hook's refusal), unreachable, timed out —
    /// classified as a fetch failure is. The run exits `1`.
    PushFailed { failure: RemoteFailure },
    /// The action ran and git refused it (`message`, git's words), or it
    /// couldn't run: the run exits `1`.
    Failed { action: SyncAction, message: String },
}

impl BranchOutcome {
    /// Whether the action failed, so the run exits `1`.
    pub const fn failed(&self) -> bool {
        matches!(self, Self::PushFailed { .. } | Self::Failed { .. })
    }
}

/// What held a branch's action: the verdict's hold (`HeldBy`, the same
/// names), or one found at the moment of acting, when sync re-checks what
/// the action relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncHold {
    Pinned,
    Entry,
    /// A push through `origin` wouldn't reach the registry's repo over SSH —
    /// as classified, or found when sync re-read the push URLs right before
    /// pushing (the push itself goes to the registry's URL, never through
    /// origin).
    PushUrl,
    /// Never in a sync document's outcomes — a held refresh isn't one — but
    /// named so every `HeldBy` converts.
    OriginNotHttps,
    FetchFailed,
    /// The checkout the branch is on has uncommitted changes — as
    /// classified, or found when sync came to act.
    DirtyCheckout,
    UnprobedWorktree,
    SeveralCheckouts,
    /// A live session works in the checkout the branch is on — as
    /// classified, or found when sync re-read the sessions right before
    /// acting.
    Busy,
    /// A live session may work there unseen — as classified, or found so
    /// right before acting (busy detection unavailable, among them).
    BusyUnknown,
    /// Found at the moment of acting: the branch or its checkout isn't as
    /// the probe read it — the checkout's HEAD left the branch, a shallow
    /// branch gained commits on no remote, a branch to move in place is
    /// checked out now, a branch to update in place became a symbolic ref,
    /// a branch to push holds another commit or upstream than classified or
    /// is no longer ahead of it — or the remote's branch moved or was
    /// deleted since the fetch, so the push's lease refused it (or, for a
    /// branch `repos push --new-branch` creates, created there since); or a
    /// partial clone's origin, read again as git resolves it right before
    /// the checkout, no longer names the registry's repo over the transport
    /// its lazy fetch was decided on; or a missing dir to clone into is
    /// there now. Rerun to reclassify.
    Changed,
}

impl From<HeldBy> for SyncHold {
    fn from(by: HeldBy) -> Self {
        match by {
            HeldBy::Pinned => Self::Pinned,
            HeldBy::Entry => Self::Entry,
            HeldBy::PushUrl => Self::PushUrl,
            HeldBy::OriginNotHttps => Self::OriginNotHttps,
            HeldBy::FetchFailed => Self::FetchFailed,
            HeldBy::DirtyCheckout => Self::DirtyCheckout,
            HeldBy::UnprobedWorktree => Self::UnprobedWorktree,
            HeldBy::SeveralCheckouts => Self::SeveralCheckouts,
            HeldBy::Busy => Self::Busy,
            HeldBy::BusyUnknown => Self::BusyUnknown,
        }
    }
}

/// The `repos push --json` document: the state the push acted on, and what
/// it did with each target.
#[derive(Debug, Clone, Serialize)]
pub struct PushReport {
    /// `PUSH_FORMAT_VERSION`.
    pub version: u32,
    /// What the push acted on: a `status --fetch` report of the targets'
    /// entries after the fetch (so `fetched` is true), in the order the
    /// targets first name them, each branch classified with the live
    /// sessions read after the fetch — every branch the entry has, though
    /// only the one checked out at each target is acted on. No unregistered
    /// scan runs (`unregistered` is `null`). Parsed with the status
    /// report's own schema: its `version` is `STATUS_FORMAT_VERSION`.
    pub status: StatusReport,
    /// What the push did, one per target checkout, in the order given
    /// (each checkout once).
    pub pushes: Vec<CheckoutPush>,
}

impl PushReport {
    pub const fn new(status: StatusReport, pushes: Vec<CheckoutPush>) -> Self {
        Self {
            version: PUSH_FORMAT_VERSION,
            status,
            pushes,
        }
    }

    /// Whether every target's branch ended in sync with its upstream —
    /// pushed, created, or already there — so the run exits `0`; anything
    /// else (held, not ahead, left to a person, no upstream, a remote
    /// branch found in the way, a detached HEAD, a checkout not read, a
    /// push that failed) exits `1`, as `git push` does on a rejected ref.
    pub fn in_sync(&self) -> bool {
        self.pushes.iter().all(|p| p.outcome.in_sync())
    }
}

/// What `repos push` did with one target: the branch checked out there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckoutPush {
    /// The entry the target names.
    pub key: String,
    /// The checkout: a path target's, the top level of the checkout holding
    /// it (a linked worktree's own); a key's or dir name's, the entry's dir.
    pub checkout: String,
    /// The branch checked out there; `None` when HEAD is detached or the
    /// checkout wasn't read.
    pub branch: Option<String>,
    /// How the entry's fetch went.
    pub fetch: FetchOutcome,
    /// Flattened: the outcome's `kind` tag and its payload sit beside
    /// `branch`.
    #[serde(flatten)]
    pub outcome: PushOutcome,
}

/// What `repos push` did with a target's branch. The branch's relation and
/// verdict are in the status report's entry, under the same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PushOutcome {
    /// Pushed to its upstream on the registry's repo, as `sync` pushes:
    /// the remote's branch moved from `from`, the fetched tip, to `to`.
    Pushed { from: String, to: String },
    /// Under `--new-branch`: created on the registry's repo at `to`, under
    /// its own name, and made the branch's upstream as `git push -u` does
    /// — or found there already at `to` (another hand's, or an earlier
    /// run's that stopped before setting the upstream), its upstream set
    /// all the same.
    Created { to: String },
    /// Nothing to push: the branch is at its upstream's tip, as just
    /// fetched from the registry's repo, or found already there when pushed
    /// (another hand's push since the fetch).
    InSync,
    /// Ahead, but `by` held the push — the verdict's hold, or one found
    /// right before pushing (the same holds as `sync`'s).
    Held { by: SyncHold },
    /// The push reached for the remote and failed there (as `sync`'s).
    PushFailed { failure: RemoteFailure },
    /// The push couldn't run, or git refused it (`message`, git's words).
    Failed { message: String },
    /// Not ahead of its upstream: behind it, or a shallow branch whose tip
    /// differs with nothing local — `repos sync` fast-forwards or moves it.
    NotAhead,
    /// Left to a person (diverged, archived, …): never pushed.
    NeedsHuman { reason: BranchNeedsHuman },
    /// No upstream on origin to push to — none set, another remote's, or
    /// one deleted on origin (`gone`) — and a push creates a remote branch
    /// only under `--new-branch`, the user's: none set, or a same-named one
    /// gone.
    NoUpstream,
    /// Under `--new-branch`, a branch with no upstream whose name the fetch
    /// found on origin, at `at`: `--new-branch` creates a branch, never
    /// overwrites or adopts one — its upstream is a person's to set.
    RemoteBranchExists { at: String },
    /// HEAD is detached: no branch to push.
    Detached,
    /// The checkout wasn't read: the entry is missing or not a repo, its
    /// probe failed (the status entry's `probe_error`), or the checkout is
    /// a worktree the probe couldn't read.
    Unread,
}

impl PushOutcome {
    /// Whether the branch ends in sync with its upstream.
    pub const fn in_sync(&self) -> bool {
        matches!(
            self,
            Self::Pushed { .. } | Self::Created { .. } | Self::InSync
        )
    }
}
