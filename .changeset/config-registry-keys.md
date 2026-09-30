---
'@fuzdev/fuz_gitops': minor
---

feat: the gitops tasks read repo state from the `repos` binary (`repos status <keys…> --json`), and `gitops.config.ts` lists `repos.toml` registry keys (breaking)

- `gitops.config.ts` is `{repos: Array<string>}` of registry keys (`GitopsConfig`, or a `CreateGitopsConfig` function taking no arguments); URLs, object entries, `repos_dir`, and `GitopsRepoConfig` are gone — each repo's dir, URL, branch, visibility, `ci`, and `archived` come from the registry
- the tasks need the `repos` binary on `PATH` (`cargo install --path crates/fuz_repos --locked` from a fuz_gitops checkout), and take `--registry <path>`; `--dir` and `gitops_sync --download` are removed (`repos sync <key>` clones a missing repo)
- a configured key that's unknown, a third-party reference, missing, not a repo, or unprobed fails every task, naming each; `gitops_run` no longer skips missing repos
- `LocalRepo` and `LocalRepoPath` carry the registry `entry` (`ReposEntryStatus`) in place of `repo_config` and `repo_git_ssh_url`; `LocalRepoMissing` is gone
- adds `resolve_gitops_repos`, `local_repos_resolve`, `repos_status_load.ts`, and `ReposOperations`; removes `get_repo_paths`, `local_repos_ensure`, `local_repo_locate`, `resolve_gitops_paths`, `resolved_gitops_config.ts`, `config_reconcile.ts`, `paths.ts`, `normalize_gitops_config`, and `create_empty_gitops_config`
- `get_gitops_ready` drops its `dir` and `download` options and gains `registry`, `host` (the public-host guard), and `repos_ops`; it no longer returns `repos_dir`
- `gitops_config_leaked_private_repos` takes registry entries
