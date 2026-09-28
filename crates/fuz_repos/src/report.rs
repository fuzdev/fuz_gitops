//! The `repos status` report — the `--json` document and what the text
//! renderer reads.

use serde::Serialize;

use crate::STATUS_FORMAT_VERSION;
use crate::classify::NeedsHuman;
use crate::registry::{CheckoutMode, EntryKind, Visibility};
use crate::state::{BranchStatus, Checkout, Layout, Presence};

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
    /// Clones under the workspace root the registry doesn't claim; `None`
    /// when the scan didn't run.
    // TODO: the unregistered scan (pass 2); always `None` until then
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

/// A clone under the workspace root that no registry entry claims.
#[derive(Debug, Clone, Serialize)]
pub struct UnregisteredClone {
    pub dir: String,
    pub origin: String,
    /// Whether the origin's account is one of the registry's owners.
    pub owned: bool,
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
    /// Under `--fetch`, git's message when the fetch failed.
    // TODO: slice 2 classifies fetch failures from stderr (a missing remote
    // ref is the upstream gone; auth, host-key, and connection errors are the
    // host unreachable), likely turning this into a `kind`-tagged enum
    pub fetch_error: Option<String>,
}
