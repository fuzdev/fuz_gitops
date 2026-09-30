//! Helpers shared by the `status_remote_*` tests.

use fuz_repos::classify::{NeedsHuman, OriginFix, OriginRemote};
use fuz_repos::report::EntryStatus;

/// An entry's origin drift, if any.
pub fn drift(e: &EntryStatus) -> Option<(OriginRemote, OriginFix)> {
    e.needs_human.iter().find_map(|r| match r {
        NeedsHuman::OriginMismatch { origin, fix, .. } => Some((origin.clone(), fix.clone())),
        _ => None,
    })
}
