---
'@fuzdev/fuz_gitops': minor
---

feat: `gitops_sync` reads each repo as it sits and refuses one not ready to generate from, instead of switching branches, pulling, and installing (breaking)

- `gitops_sync` never switches a branch, pulls, or installs: it resolves the repos from `repos status <keys…> --json`, refuses (before any network, naming each with its fix) a repo off its registry branch, dirty (untracked files count), or mid-operation, then fetches them (`repos status --fetch`, remote-tracking refs only) and warns on a followed branch not in sync with origin, a failed fetch, and an npm repo with no `node_modules` or no `.svelte-kit/tsconfig.json` its tsconfig extends (the library analysis then reads external types as `any`)
- `--allow_dirty` now reads repos off their branch, dirty, or mid-operation as they sit, warning instead of refusing
- `--check` is the readiness report alone, from local refs: no fetch, no `SECRET_GITHUB_API_TOKEN`, nothing written, and a non-zero exit when a real run would refuse
- adds `repo_readiness_for_gen` and `check_gen_readiness` to `repo_readiness.ts`, and `prepare_gitops_sync`; the publish readiness message alone points a dirty repo at the troubleshooting doc
- `get_gitops_ready`, `local_repos_load`, and `local_repo_load` drop their `sync`, `allow_dirty`, `git_ops`, and `npm_ops` options, loading each repo as it sits
- `GitOperations` keeps `current_commit_hash`, `add`, and `commit`: `current_branch_name`, `check_clean_workspace`, `checkout`, `pull`, `switch_branch`, `has_remote`, `add_and_commit`, `has_changes`, `tag`, `push_tag`, `stash`, `stash_pop`, and `has_file_changed` are removed, with `git_add_and_commit`, `git_tag`, `git_push_tag`, `git_has_changes`, `git_has_file_changed`, `git_stash`, `git_stash_pop`, `git_switch_branch`, `git_current_branch_name_required`, `git_check_clean_workspace_as_boolean`, and `git_has_remote` from `git_operations.ts`
- removes `NpmOperations.install`
