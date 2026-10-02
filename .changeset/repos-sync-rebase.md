---
'@fuzdev/fuz_repos': minor
---

feat: `repos sync` rebases a diverged registry branch onto its fetched upstream and pushes it, stopping with nothing moved on any conflict (breaking: the `repos status --json` format is 19 — reinstall the `repos` binary with `cargo install --path crates/fuz_repos --locked`)

- a branch's `verdict` action may be `rebase` (`ahead`, `behind`), and a branch left to a person may read `diverged_published`, `diverged_merge`, or `diverged_tagged`; `ReposSyncAction` and `ReposBranchNeedsHuman` in `repos_status.ts` carry them
- the readiness fix for a diverged branch names `repos sync <key>` before the by-hand rebase or merge; a diverged branch still isn't ready to publish or in sync with origin
