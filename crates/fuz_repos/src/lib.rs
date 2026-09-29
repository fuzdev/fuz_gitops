//! `fuz_repos` — deterministic git operations over the repos a `repos.toml`
//! registry declares.
//!
//! The library holds the registry types, the hardened git runner, the probe,
//! the unregistered scan, the pure classification, and what a remote's
//! answers mean (fetch failures, the visibility check); the `repos` binary
//! parses arguments, renders reports, and owns exit codes.

pub mod classify;
pub mod discover;
pub mod error;
pub mod git;
pub mod porcelain;
pub mod probe;
pub mod registry;
pub mod remote;
pub mod report;
pub mod scan;
pub mod state;
pub mod status;
pub mod url;

/// The version of the `repos status --json` document. Bumped on any breaking
/// change to its shape (a removal, a rename, a changed meaning); additions
/// don't bump it.
pub const STATUS_FORMAT_VERSION: u32 = 4;
