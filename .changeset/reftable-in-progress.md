---
'@fuzdev/fuz_repos': patch
---

fix: `repos` reads a cherry-pick or revert in progress in a reftable repo, which it read as idle, so `repos sync` and the publish readiness gate no longer treat such a checkout as at rest (reinstall the `repos` binary with `cargo install --path crates/fuz_repos --locked`)
