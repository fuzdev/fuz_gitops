---
'@fuzdev/fuz_repos': minor
---

feat: `repos sync` rebases a diverged registry branch onto its fetched upstream and pushes it, and `repos push` does the same for the branch it's asked to push, stopping with nothing moved on any conflict (breaking: the `--json` formats are `repos status` 19, `repos sync` 13, and `repos push` 11 — reinstall the `repos` binary with `cargo install --path crates/fuz_repos --locked`)

- only the registry's branch of an owned entry, neither archived nor pinned, tracking origin's branch of the same name outside a partial clone, is rebased, and only when its commits ahead are on no remote, with no merge or tag among them; a branch's `verdict` action may be `rebase` (`ahead`, `behind`), and any other diverged branch stays a person's, reading `diverged`, `diverged_published`, `diverged_merge`, or `diverged_tagged` (`ReposSyncAction` and `ReposBranchNeedsHuman` in `repos_status.ts`)
- whatever holds a fast-forward or a push holds the rebase: a diverged branch in a dirty checkout (untracked files count) is held, nothing moved — commit, or `git stash -u`, and run again; `repos push` still pushes a branch ahead from a dirty checkout
- a rebase commits as you: with no `user.name` and `user.email` configured, it fails saying to set them
- nothing settles a conflict: no merge driver or `rerere` runs during the replay, so a path a `merge=union` (or any other driver) attribute would have merged conflicts and stops the rebase; `repos sync` exits `0` (the branch is a person's), `repos push` exits `1`, as any push that didn't land
- the reports say what moved: `repos push`'s `rebased` line names how many upstream commits the branch now sits on, its new tip, and the tip replaced; in `--json`, a branch outcome may be `rebased` (`from`, `to`, `onto`, `push`) or `rebase_refused` (`why`)
- the readiness fix for a diverged branch names `repos sync <key>` before the by-hand rebase or merge; a diverged branch still isn't ready to publish or in sync with origin
