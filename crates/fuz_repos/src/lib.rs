//! `fuz_repos` — deterministic git operations over the repos a `repos.toml`
//! registry declares.
//!
//! The library holds the registry types, the hardened git runner, git's
//! own files read as git reads them (`gitdir`), the probe, the unregistered
//! scan, the pure classification, what a remote's answers mean (fetch
//! failures, the visibility check), busy detection — the live Claude
//! Code sessions (`sessions`) and the checkouts they sit in (`busy`) —
//! `sync`, which carries out the verdicts (a missing entry's through
//! `clone`), and `push`, which carries out one checkout's branch's push
//! through sync's own. The `repos` binary parses arguments, renders
//! reports, and owns exit codes.
//!
//! **What it writes.** The tool moves refs it didn't author and reports git
//! state: it fetches, fast-forwards, moves shallow branches with no local
//! commits, clones, and pushes commits that already exist. It never makes a
//! commit or a tag, merges anything but a fast-forward, force-pushes,
//! deletes a branch, or prunes a worktree. `status` writes nothing, and
//! `status --fetch`'s fetch writes remote-tracking refs and what a fetch
//! needs behind them (objects, `FETCH_HEAD`, the shallow boundary) — never
//! a tag; `sync` writes the branch it acts on, the checkout that branch is
//! on, and new clones; a push writes one remote branch of an owned entry,
//! under a lease (on the fetched tip, or on none for `--new-branch`), then
//! its remote-tracking ref (and, for `--new-branch`, the upstream config).
//! Authoring content — commits, changesets,
//! release tags — and package meaning (npm, the dependency graph, the
//! GitHub API) are left to the tools around it.
//!
//! Unix-only: it takes git's paths as raw bytes, as git does. Busy
//! detection reads `/proc`, so it works on Linux alone; elsewhere, with any
//! session recorded, it fails closed.

pub mod busy;
pub mod classify;
pub mod clone;
pub mod discover;
pub mod error;
pub mod git;
mod gitdir;
pub mod porcelain;
pub mod probe;
pub mod push;
pub mod registry;
mod regular_file;
pub mod remote;
pub mod report;
pub mod scan;
pub mod sessions;
pub mod state;
pub mod status;
pub mod sync;
pub mod url;

/// The version of the `repos status --json` document. Bumped on any change
/// to its shape, new fields and variants included: consumers parse it with
/// strict objects and closed unions.
pub const STATUS_FORMAT_VERSION: u32 = 15;

/// The version of the `repos sync --json` document. Bumped on any change to
/// its shape, the embedded status report's included (so with every
/// `STATUS_FORMAT_VERSION` bump).
pub const SYNC_FORMAT_VERSION: u32 = 9;

/// The version of the `repos push --json` document. Bumped on any change to
/// its shape, the embedded status report's included (so with every
/// `STATUS_FORMAT_VERSION` bump).
pub const PUSH_FORMAT_VERSION: u32 = 5;
