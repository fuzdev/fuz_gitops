---
'@fuzdev/fuz_repos': patch
---

feat: `repos push` rebases the diverged registry branch it's asked to push onto its fetched upstream, then pushes it — the rebase `repos sync` makes, for the one branch checked out at the target (the `repos push --json` format is 11 — reinstall the `repos` binary with `cargo install --path crates/fuz_repos --locked`)

- a diverged branch in a dirty checkout (untracked files count) is held, nothing moved: commit, or `git stash -u`, then `repos push` again; a branch ahead still pushes from a dirty checkout
- the report says what moved: a `rebased` line with the upstream commits the branch now sits on, its new tip, and the tip replaced; in `--json`, a `rebased` outcome (`from`, `to`, `onto`, `push`) or `rebase_refused` (`why`)
- a conflict stops the rebase with nothing moved and exits `1`, as any push that didn't land
