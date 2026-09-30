---
'@fuzdev/fuz_gitops': minor
---

feat: `gitops_publish --wetrun` gates on repo readiness instead of syncing; the diagnostics drop `--sync` (breaking)

- a real publish no longer switches branches, pulls, or installs: after the plan and before the confirmation prompt it runs `repos status <keys…> --fetch --json` on every npm repo and refuses unless each is on its registry branch, clean (untracked files count), idle, in sync with origin or ahead of it (logged: its release push carries those commits, or they stay unpushed), fetched without error, free of other live sessions in its checkout, and without `needs_human` reasons — naming each problem and its fix, and changing nothing
- the executor re-checks each repo the same way (`repos status --fetch --json <key>`) right before its `gro publish`, aborting before any npm side effect with the new `not_ready` failure code unless it's still ready
- `gitops_analyze`, `gitops_plan`, `gitops_validate`, and `gitops_publish` drop `--sync` (`repos sync` moves repos) and print a readiness block, as warnings, naming each npm repo not at rest
- the executor runs `gro publish --no-build --no-pull --branch <entry branch>`
- preflight no longer reads git: its clean-workspace, branch, and git-remote checks are gone, with `PreflightOptions.required_branch` and `check_remote`, and `run_preflight_checks` and `PreflightOperations` no longer take `git_ops`
- removes `GitOperations.list_uncommitted_files` and `git_list_uncommitted_files`
- `ReposOperations.status` and `load_repos_status` take `fetch`; `GitopsOperations` gains `repos`, and `PublishingOptions` gains `registry`
- adds `repo_readiness.ts` (`repo_readiness_at_rest`, `repo_readiness_for_publish`, `check_publish_readiness`, `repos_not_at_rest`, `format_readiness_block`, `format_repo_readiness_problem`, `format_readiness_ahead`), `gate_publish_readiness` and `log_readiness_block` in `gitops_task_helpers.ts`, and `gro_publish_args`
