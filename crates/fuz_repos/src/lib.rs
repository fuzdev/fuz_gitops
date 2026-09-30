//! `fuz_repos` — deterministic git operations over the repos a `repos.toml`
//! registry declares.
//!
//! **Load a registry, get a report.** Each command is one call:
//! `status::status_report`, `sync::sync_report`, and `push::push_report`
//! take a git runner (`git::Git`), the cwd, where the registry is
//! (`discover::Locate`; the default finds it walking up from the cwd), the
//! targets, and the command's options, and return the finished document
//! `repos --json` prints (`report::StatusReport`, `SyncReport`,
//! `PushReport`) with the run's timings. The run's policy is theirs: which
//! references it refreshes, when the unregistered scan runs, and what of
//! it the report carries, so any caller gets the binary's verdicts.
//! `status::checkout_status` is `status --brief`'s probe of the one
//! checkout holding a path; `discover::Workspace::load` is the loading
//! alone.
//!
//! Beneath them, public for a caller driving one phase (and for the
//! integration tests): the registry (`registry`: parsed, then validated
//! into the `ValidRegistry` that alone yields entries), finding it and
//! resolving targets (`discover`), the unregistered scan (`scan`), the
//! runs over resolved entries (`status::status`, `sync::sync`,
//! `push::push`, a missing entry's clone through `clone`), and the live
//! Claude Code sessions busy detection reads (`sessions`). The report's
//! vocabulary is `report`, `state`, `classify` (entry-level reasons,
//! `Refresh`), `remote` (what a remote's answers mean), and `error`. The
//! probe, busy detection's scoping, URL parsing and redaction (`url`), path
//! resolution (`paths`), and git's files and output formats are private.
//! The `repos` binary parses arguments, renders reports, and owns exit
//! codes.
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

mod busy;
pub mod classify;
pub mod clone;
pub mod discover;
pub mod error;
pub mod git;
mod gitdir;
mod paths;
mod porcelain;
mod probe;
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
mod url;

/// The version of the `repos status --json` document. Bumped on any change
/// to its shape, new fields and variants included: consumers parse it with
/// strict objects and closed unions.
pub const STATUS_FORMAT_VERSION: u32 = 17;

/// The version of the `repos sync --json` document. Bumped on any change to
/// its shape, the embedded status report's included (so with every
/// `STATUS_FORMAT_VERSION` bump).
pub const SYNC_FORMAT_VERSION: u32 = 11;

/// The version of the `repos push --json` document. Bumped on any change to
/// its shape, the embedded status report's included (so with every
/// `STATUS_FORMAT_VERSION` bump).
pub const PUSH_FORMAT_VERSION: u32 = 7;
