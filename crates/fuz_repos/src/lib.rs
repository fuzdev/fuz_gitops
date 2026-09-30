//! `fuz_repos` — deterministic git operations over the repos a `repos.toml`
//! registry declares.
//!
//! The library holds the registry types, the hardened git runner, git's
//! own files read as git reads them (`gitdir`), the probe, the unregistered
//! scan, the pure classification, what a remote's answers mean (fetch
//! failures, the visibility check), busy detection — the live Claude
//! Code sessions (`sessions`) and the checkouts they sit in (`busy`) — and
//! `sync`, which carries out the verdicts (a missing entry's through
//! `clone`); the `repos` binary parses
//! arguments, renders reports, and owns exit codes.
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
pub const STATUS_FORMAT_VERSION: u32 = 11;

/// The version of the `repos sync --json` document. Bumped on any change to
/// its shape, the embedded status report's included (so with every
/// `STATUS_FORMAT_VERSION` bump).
pub const SYNC_FORMAT_VERSION: u32 = 4;
