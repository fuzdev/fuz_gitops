//! `fuz_repos` — deterministic git operations over the repos a `repos.toml`
//! registry declares.
//!
//! The library holds the registry types, the hardened git runner, the probe,
//! and the pure classification; the `repos` binary parses arguments, renders
//! reports, and owns exit codes.

/// The version of the `repos status --json` document. Bumped on any breaking
/// change to its shape (a removal, a rename, a changed meaning); additions
/// don't bump it.
pub const STATUS_FORMAT_VERSION: u32 = 1;
