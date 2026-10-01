//! Helpers shared by the `sync*` tests: reading a run's outcomes.

use fuz_repos::report::{BranchOutcome, EntrySync};
use fuz_repos::sync::SyncRun;

/// What the run did for the entry `key`.
pub fn outcomes<'a>(run: &'a SyncRun, key: &str) -> &'a EntrySync {
    run.outcomes
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no outcomes for {key}: {:?}", run.outcomes))
}

/// What the run did to the entry `key`'s branch `name`.
pub fn outcome<'a>(run: &'a SyncRun, key: &str, name: &str) -> &'a BranchOutcome {
    &outcomes(run, key)
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch {key}:{name}: {:?}", run.outcomes))
        .outcome
}
