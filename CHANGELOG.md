# @fuzdev/fuz_gitops

## 0.79.0

### Minor Changes

- feat: the gitops tasks read repo state from the `repos` binary and never move a repo; `gitops.config.ts` lists `repos.toml` registry keys (breaking) ([#59](https://github.com/fuzdev/fuz_repos/pull/59))

  **Config and the `repos` binary**

  - `gitops.config.ts` exports `{repos: Array<string>}` of registry keys, each listed once (or a no-argument `CreateGitopsConfig` returning it); each repo's dir, URL, branch, visibility, `ci`, and `archived` come from the registry — URL and object entries and `repos_dir` are gone
  - the tasks need the `repos` binary on `PATH` (`cargo install --path crates/fuz_repos --locked` from a fuz_repos checkout) and a registry it finds from the cwd, or `--registry <path>`; `--dir` and `gitops_sync --download` are removed (`repos sync <key>` clones a missing repo)
  - every task fails, naming each problem, on an empty config or a key that's unknown, a third-party reference, missing, not a repo, or unprobed; `gitops_run` no longer skips missing repos
  - `gitops_sync` refuses to run when a public host package's config lists repos the registry declares private (`gitops_config_leaked_private_repos`)

  **No task moves a repo**

  - no task switches a branch, pulls, or installs: `gitops_analyze`, `gitops_plan`, `gitops_validate`, and `gitops_publish` drop `--sync` (`repos sync` moves repos), read repos as they sit, and warn naming each npm repo not at rest
  - `gitops_sync` refuses, before any network, a repo off its registry branch, dirty, or mid-operation (`--allow_dirty` reads them as they sit, warning), then fetches and warns on a branch not in sync with origin, a failed fetch, or an npm repo missing `node_modules` or `.svelte-kit/tsconfig.json`; `--check` is that readiness report from local refs — no fetch, no `SECRET_GITHUB_API_TOKEN`, nothing written — exiting non-zero when a run would refuse
  - `gitops_publish --wetrun` gates instead of syncing: before the confirmation prompt it runs `repos status --fetch` and refuses, changing nothing, unless every npm repo is on its registry branch, clean (untracked files count), with no operation in progress, in sync with origin or ahead, fetched without error, free of other live Claude Code sessions with busy detection available, and has no `needs_human` reason
  - the executor re-checks each repo right before its `gro publish`, aborting before any npm side effect with the new `not_ready` failure code

  **Task output**

  - under `--emit_json`, or `--format json` or `markdown` without `--outfile`, `gitops_analyze`, `gitops_plan`, `gitops_publish`, and `gitops_run --format json` send their log to stderr — the plan, the readiness block and gate, the executor's progress, and gro's lines after the task — so stdout carries the document or the JSON-lines events alone (gro's two lines before the task runs still lead it); `gro publish` and `gro deploy` send their stdout to stderr then too, and `gitops_publish`'s confirmation prompt is always on stderr
  - `gitops_run --format json` reports a command killed by a signal, or one that never started, with `exit_code: null` (it said `0` for a signal, `-1` for a spawn error), and each result gains `signal`; the text format says `Killed by <signal>`; a runner that throws is reported under its own repo rather than `unknown`

  **Publishing executor**

  - the executor runs `gro publish --no-build --no-pull --branch <entry branch>`, and its dependency-update commits take only the `package.json` and changeset they staged
  - `gro publish` and `gro deploy` run in the foreground — output live, stdin the terminal's so npm can prompt for a one-time password
  - a failed step's message says why on its first line, which the log shows alone, then the end of its stderr, secrets redacted (the markdown report fences it); a failed `gro publish`'s message drops its `Failed to publish <pkg>:` prefix
  - preflight takes the plan and checks no git state (clean workspace, branch, remote — the readiness gate's now): it builds exactly the packages the plan publishes — auto-generated ones too, which it skipped before though `gro publish --no-build` then published them unbuilt — and no others, reads no changesets, drops the per-repo "has no changesets" warnings and the publish-time estimate, and decides its npm registry check by `npm ping`'s exit status, no longer warning on every run; a build logs its "Building" line once
  - secrets are masked in every `--emit_json` event, in the `--format json` report's `events`, and in the `--format markdown` report's failures
  - `gitops_publish --peer_strategy` accepts `gte`, and its help says what it sets: the prefix of a rewritten dependency of any type whose range has none (a wildcard still becomes `^`)

  **Plan**

  - `PublishingPlan` gains `no_changes`, the package names with nothing to publish, and its `info` carries only informational sentences (excluded non-npm repos, dev dependency cycles); `gitops_plan`'s JSON gains `no_changes`, and its markdown lists the info sentences apart from the packages
  - one classification of a version change (`version_change_kind`: `explicit`, `escalation`, `auto`) is shared by the plan's log, markdown, and `--preview`, so the preview labels a publish `explicit` or `auto` where it said `changeset` or `auto_changeset`
  - an auto-generated version change that a later pass raises stays auto, with `needs_bump_escalation`, `existing_bump`, and `required_bump` unset: a dependent listed before its dependency is no longer reported as an escalation too, double-listed in the plan; an escalation's `existing_bump` stays the changesets' bump across passes
  - the plan warns, naming the repo and why, on changeset files that yield no bump for their package — none parses, or none that parses names it — where it skipped the repo silently; its max-iterations warning names the packages one more pass would change in place of a made-up estimate of the iterations left, and no longer fires when the last allowed pass converged the plan
  - `gitops_plan`'s markdown marks breaking changes in a "Breaking" column and summary count, pre-1.0 breaking bumps included, where a "Major" column counted only major bumps

  **Dashboard**

  - `RepoJson` gains an optional `branch`, which `gitops_sync` writes as the registry branch it fetched CI status for, and `Repo` gains `branch` (`main` when the data has none); the table's CI link and the modules page's file links point at that branch rather than `main`
  - `fetch_repo_data` skips the check-runs request for a repo whose registry entry has `ci: false`, and no longer logs "failed to fetch CI status" for a branch with no check runs — only for a failed request, now with its status
  - the pull requests page and `to_pull_requests` list the pull requests of repos without a `homepage` (a cargo repo like `tsv`), which they dropped
  - the pull requests page keys each row by its repo and number, so pull requests sharing a number across repos no longer collide, and the modules nav marks the repo named in the URL hash as selected, which it never matched
  - `PageHeader`'s `repo` prop drops its `{url; pkg_json: null}` variant; the page components no longer wrap `PageFooter` in a second `section`; `ReposTable`, `ReposTree`, `ReposTreeNav`, and `ModulesDetail` drop the branches for a repo without a `package_json` or `repo_url`, which a loaded repo always has (including `ReposTree`'s "failed to load library metadata" summary)
  - the generated `repos.ts` imports `RepoJson` from `repo.svelte.ts` rather than `repo.svelte.js`

  **API (breaking), by module**

  - `gitops_config.ts`
    - `GitopsConfig` is a zod schema as well as a type; adds `parse_gitops_config` and `gitops_config_leaked_private_repos`
    - removes `GitopsRepoConfig`, `RawGitopsConfig`, `RawGitopsRepoConfig`, `GitopsRepoVisibility`, `normalize_gitops_config`, and `create_empty_gitops_config`
  - `local_repo.ts`
    - `LocalRepo` and `LocalRepoPath` carry the registry `entry` (`ReposEntryStatus`) in place of `repo_config` and `repo_git_ssh_url`; `LocalRepoPath` drops `type`, and its `repo_name` is the registry key
    - `local_repos_load` and `local_repo_load` drop `sync`, `allow_dirty`, `git_ops`, and `npm_ops` and load each repo as it sits (both synced by default before)
    - adds `local_repos_resolve`; removes `local_repos_ensure`, `local_repo_locate`, and `LocalRepoMissing`
  - `gitops_task_helpers.ts`
    - `get_gitops_ready` takes `ResolveGitopsReposOptions` (`config`, `registry`, `host`, `log`, `repos_ops`) in place of `GetGitopsReadyOptions`, and returns only `local_repos`
    - adds `resolve_gitops_repos`, `gate_publish_readiness` with `GatePublishReadinessOptions`, and `log_readiness_block`
    - removes `import_gitops_config`, `resolve_gitops_paths`, and `ResolveGitopsPathsOptions`
  - `operations.ts` / `operations_defaults.ts`
    - `GitopsOperations` gains `repos`: adds `ReposOperations` and `ReposCommandOutput` (`operations.ts`) and `default_repos_operations` (`operations_defaults.ts`)
    - `ProcessOperations.spawn` is replaced by `run_interactive` (its `stdout` option routes the child's stdout to ours or our stderr; a failure carries `stderr_tail`, the end of its stderr, cut by `output_tail` to `OUTPUT_TAIL_MAX_LINES` and `OUTPUT_TAIL_MAX_CHARS`, all three added to `operations_defaults.ts`)
    - `GitOperations` keeps only `current_commit_hash` (without `branch`), `add`, and `commit` (which takes the `files` to commit): removes `current_branch_name`, `check_clean_workspace`, `checkout`, `pull`, `switch_branch`, `has_remote`, `add_and_commit`, `has_changes`, `list_uncommitted_files`, `tag`, `push_tag`, `stash`, `stash_pop`, and `has_file_changed`
    - `NpmOperations.install` is removed, and `NpmOperations.wait_for_package`'s failure drops `timeout`
    - `BuildOperations.build_package` drops `log`
    - `PreflightOperations.run_preflight_checks` takes `RunPreflightChecksOptions`, without `git_ops`
  - `git_operations.ts`
    - `git_commit` takes the files to commit, and `git_current_commit_hash_required` drops its `branch` parameter
    - removes `git_add_and_commit`, `git_tag`, `git_push_tag`, `git_has_changes`, `git_has_file_changed`, `git_list_uncommitted_files`, `git_stash`, `git_stash_pop`, `git_switch_branch`, `git_current_branch_name_required`, `git_check_clean_workspace_as_boolean`, and `git_has_remote`
  - `preflight_checks.ts`
    - `run_preflight_checks` takes the plan's `version_changes` and drops `git_ops`; `RunPreflightChecksOptions` drops `changeset_ops` and `preflight_options` for a top-level `log`
    - `PreflightOptions` is removed (`skip_changesets`, `skip_build_validation`, `estimate_time`, and the `required_branch` and `check_remote` it had)
    - `PreflightResult` drops `repos_with_changesets`, `repos_without_changesets`, `estimated_duration`, and `npm_username`
  - `multi_repo_publisher.ts`
    - `PublishingOptions` gains `registry` and `child_stdout`
  - `npm_registry.ts`
    - removes `get_package_info`, `package_exists`, and `PackageInfo`
    - `check_package_available` and `wait_for_package` take a trailing `NpmRegistryDeps` (`run_npm`, `wait`, `now`), defaulting to `default_npm_registry_deps`
  - `publishing_event.ts` / `publishing_event_handler.ts`
    - `PublishingErrorCode` gains `not_ready` and drops `auth`, `dependency`, `build`, and `other`, which were never emitted
    - removes `null_handler`; `masking_handler` drops its `mask` parameter, always masking with `mask_secrets`; `stdout_handler` takes an optional line writer
  - `publishing_plan.ts` / `publish_steps.ts`
    - adds `version_change_kind` and `VersionChangeKind`; `PublishStep`'s `via` is a `VersionChangeKind`, and `PublishStepVia` is removed
    - `publishing_plan.ts` no longer re-exports `log_publishing_plan` and `LogPlanOptions`; import them from `publishing_plan_logging.ts`
  - `dependency_graph.ts` / `graph_validation.ts`
    - `DependencyGraph`'s constructor takes the repos in place of `init_from_repos`, and `analyze` is its method, returning `DependencyAnalysis` (moved from `graph_validation.ts` to `dependency_graph.ts`) without `missing_peers`, which listed every external peer dependency; the formatters and loggers in `log_helpers.ts` take a `DependencyAnalysis`
    - removes `DependencyGraphBuilder` (`build_from_repos`, `analyze`, and `compute_publishing_order`, which is `graph.topological_sort(true)`), `get_dependents`, `get_dependencies`, `DependencySpec`'s `resolved`, and `DependencyNode`'s `repo` and `publishable`; `gitops_analyze`'s JSON drops each node's `publishable` (`DependencyGraphJson`) and the analysis's `missing_peers`
    - `validate_dependency_graph` takes only the repos and never throws or logs, reporting cycles and a failed sort in its result: drops the `log`, `throw_on_prod_cycles`, `log_cycles`, and `log_order` options
  - `ci_reconcile.ts`
    - `CiReconcileInput` drops `checkable`: a configured repo that isn't present fails the load
  - `fetch_repo_data.ts` / `github.ts`
    - `fetch_repo_data` takes one `FetchRepoDataOptions` object (`local_repos`, `token`, `cache`, `log`, `delay`, `github_api_version`, `fetch`) in place of six positional parameters
    - `fetch_github_check_runs` returns a `Result` whose `value` is `null` for no check runs, apart from a failure (`status`, `message`), and it and `fetch_github_pull_requests` take a `fetch` option
  - `output_helpers.ts`
    - adds `route_human_output`, `output_is_machine`, and `WriteStdout`; `OutputOptions` gains `write_stdout`
  - the task modules
    - `gitops_run.task.ts`'s `Args` and `task` are `@nodocs` like every other task's
    - the task bodies are exported, `@nodocs`, as test seams: `run_gitops_run` (`GitopsRunDeps`, with `GitopsRunOutcome`, `GitopsRunCommandOutput`, and `GitopsRunResult`), `run_gitops_plan` (`GitopsPlanDeps`), `run_gitops_analyze` (`GitopsAnalyzeDeps`), `run_gitops_publish` (`GitopsPublishDeps`, with `to_child_stdout` and `format_failure_markdown`), and `prepare_gitops_sync` (`GitopsSyncDeps`)

  **Added modules**

  - `repos_status.ts` — zod schemas for the `repos status --json` document (`ReposStatusDocument`, `ReposStatusReport`, `ReposStatusErrorReport`, `ReposEntryStatus`, and the shapes inside them) and `REPOS_STATUS_FORMAT_VERSION`
  - `repos_status_load.ts` — `load_repos_status`, `parse_repos_status_output`, `to_repos_command`, `REPOS_INSTALL_COMMAND`, and `ReposStatusLoadFailure`
  - `repo_readiness.ts` — the readiness predicates (`repo_readiness_at_rest`, `repo_readiness_for_publish`, `repo_readiness_for_gen`, `check_publish_readiness`, `check_gen_readiness`, `repos_not_at_rest`) and formatters (`format_repo_readiness_problem`, `format_readiness_ahead`, `format_readiness_block`), with `RepoReadiness`, `RepoReadinessProblem`, `RepoReadinessFormatOptions`, and `ReadinessAhead`

  **Removed modules**

  - `repo_ops.ts` (`walk_repo_files`, `collect_repo_files`, `should_exclude_path`, `get_repo_paths`, `RepoPath`, `WalkOptions`, `DEFAULT_EXCLUDE_DIRS`, `DEFAULT_EXCLUDE_EXTENSIONS`)
  - `resolved_gitops_config.ts` (`resolve_gitops_config`, `ResolvedGitopsConfig`)
  - `config_reconcile.ts` (`reconcile_configs`, `ConfigDrift`, `ConfigDriftKind`, `IntrinsicField`, `NamedRepos`): the registry is the one source of each repo's facts, so there are no configs to reconcile
  - `paths.ts` (`DEFAULT_REPOS_DIR`)

## 0.78.1

### Patch Changes

- fix: format generated JSON ([0524412](https://github.com/fuzdev/fuz_gitops/commit/0524412))

## 0.78.0

### Minor Changes

- feat: handle cargo repos ([6cab850](https://github.com/fuzdev/fuz_gitops/commit/6cab850))

## 0.77.0

### Minor Changes

- add host-state fields to repo config (groundwork) ([#49](https://github.com/fuzdev/fuz_gitops/pull/49))
  - `RawGitopsRepoConfig` accepts optional `visibility` (`'public' | 'private'`, defaults to `'public'`), `ci`, and `archived` (defaults to `false`)
  - `ci` defaults to `true` for public repos and `false` for private ones, overridable per-repo
  - `reconcile_ci` flags drift between a repo's declared `ci` and its actual workflow files, skipping archived repos
  - `gro gitops_validate` now runs `ci_reconcile` and hard-fails (throws) on any error from any step — a production dependency cycle, a plan error, or CI drift — instead of completing with a warning; warnings stay non-fatal
  - not yet consumed by sync/publish

## 0.76.0

### Minor Changes

- chore: bump fuz_ui peer dep ([36284ac](https://github.com/fuzdev/fuz_gitops/commit/36284ac))

## 0.75.0

### Minor Changes

- deps: upgrade `@fuzdev/fuz_ui@0.203.0` ([3b96872](https://github.com/fuzdev/fuz_gitops/commit/3b96872))

## 0.74.0

### Minor Changes

- chore: upgrade peer deps ([a5525f8](https://github.com/fuzdev/fuz_gitops/commit/a5525f8))

### Patch Changes

- fix: pull correct branch ([ca6f6cd](https://github.com/fuzdev/fuz_gitops/commit/ca6f6cd))

## 0.73.0

### Minor Changes

- fix: rework installing ([d661949](https://github.com/fuzdev/fuz_gitops/commit/d661949))
- feat: rework publishing flows ([2b9e284](https://github.com/fuzdev/fuz_gitops/commit/2b9e284))

## 0.72.0

### Minor Changes

- feat: publishing events ([b0a46a1](https://github.com/fuzdev/fuz_gitops/commit/b0a46a1))

## 0.71.0

### Minor Changes

- chore: upgrade peer deps ([47db18e](https://github.com/fuzdev/fuz_gitops/commit/47db18e))
- feat: add `RepoLibraryDetail.svelte` ([c155491](https://github.com/fuzdev/fuz_gitops/commit/c155491))

## 0.70.1

### Patch Changes

- fix: use `compactReplacer` in `gitops_sync.task.ts` ([c08face](https://github.com/fuzdev/fuz_gitops/commit/c08face))

## 0.70.0

### Minor Changes

- bump node@24.14 ([7dd40b7](https://github.com/fuzdev/fuz_gitops/commit/7dd40b7))
- chore: upgrade deps ([330a502](https://github.com/fuzdev/fuz_gitops/commit/330a502))

## 0.69.0

### Minor Changes

- chore: improve styling patterns ([57fbd1f](https://github.com/fuzdev/fuz_gitops/commit/57fbd1f))
- chore: rework some interfaces ([0215a5d](https://github.com/fuzdev/fuz_gitops/commit/0215a5d))
- feat: rework some interfaces and upgrade fuz_util ([5398f74](https://github.com/fuzdev/fuz_gitops/commit/5398f74))

### Patch Changes

- fix: improve some errors ([c63b1b7](https://github.com/fuzdev/fuz_gitops/commit/c63b1b7))

## 0.68.0

### Minor Changes

- upgrade peer deps ([4be343f](https://github.com/fuzdev/fuz_gitops/commit/4be343f))

## 0.67.0

### Minor Changes

- upgrade `fuz_util` and `gro` ([98f8fe1](https://github.com/fuzdev/fuz_gitops/commit/98f8fe1))

## 0.66.1

### Patch Changes

- fix gro peer dep ([afb8263](https://github.com/fuzdev/fuz_gitops/commit/afb8263))

## 0.66.0

### Minor Changes

- refactor to use fs ops pattern more ([c342a0a](https://github.com/fuzdev/fuz_gitops/commit/c342a0a))

## 0.65.2

### Patch Changes

- - replace topological sort with `@fuzdev/fuz_util/sort.js` ([9f49904](https://github.com/fuzdev/fuz_gitops/commit/9f49904))
  - remove unused `detect_cycles()` method
  - deduplicate DFS cycle detection into `#find_cycles` helper

## 0.65.1

### Patch Changes

- fix some component styles ([72226e8](https://github.com/fuzdev/fuz_gitops/commit/72226e8))

## 0.65.0

### Minor Changes

- migrate to @fuzdev/gro from @ryanatkn/gro ([7d3bd02](https://github.com/fuzdev/fuz_gitops/commit/7d3bd02))

## 0.64.0

### Minor Changes

- switch to `wetrun` ([4fdc869](https://github.com/fuzdev/fuz_gitops/commit/4fdc869))

## 0.63.0

### Minor Changes

- upgrade fuz_css ([#45](https://github.com/fuzdev/fuz_gitops/pull/45))

## 0.62.0

### Minor Changes

- upgrade deps ([#44](https://github.com/fuzdev/fuz_gitops/pull/44))

## 0.61.1

### Patch Changes

- change `gitops_sync` to output `repos.json` too ([fb2d0ca](https://github.com/fuzdev/fuz_gitops/commit/fb2d0ca))

## 0.61.0

### Minor Changes

- upgrade peer deps ([896380a](https://github.com/fuzdev/fuz_gitops/commit/896380a))

## 0.60.0

### Minor Changes

- rename some interfaces ([677f7d4](https://github.com/fuzdev/fuz_gitops/commit/677f7d4))

## 0.59.0

### Minor Changes

- add `gitops_run` task, tweak some interfaces ([#43](https://github.com/fuzdev/fuz_gitops/pull/43))

## 0.58.0

### Minor Changes

- migrate to fuzdev ([#42](https://github.com/fuzdev/fuz_gitops/pull/42))

## 0.57.0

### Minor Changes

- move to fuzdev ([248b256](https://github.com/fuzdev/fuz_gitops/commit/248b256))

## 0.56.0

### Minor Changes

- add `verbose` option and rework some interfaces ([#41](https://github.com/ryanatkn/fuz_gitops/pull/41))

## 0.55.0

### Minor Changes

- rework some interfaces ([#40](https://github.com/ryanatkn/fuz_gitops/pull/40))

## 0.54.0

### Minor Changes

- rename `PascalCase` from `Upper_Snake_Case` (lol) ([#39](https://github.com/ryanatkn/fuz_gitops/pull/39))

## 0.53.0

### Minor Changes

- support `>=` in peer deps ([#37](https://github.com/ryanatkn/fuz_gitops/pull/37))
- upgrade peer deps ([#37](https://github.com/ryanatkn/fuz_gitops/pull/37))
- upgrade deps ([#37](https://github.com/ryanatkn/fuz_gitops/pull/37))

## 0.52.0

### Minor Changes

- heal npm install cache ([#38](https://github.com/ryanatkn/fuz_gitops/pull/38)) ([aa3e917](https://github.com/ryanatkn/fuz_gitops/commit/aa3e917))

## 0.51.0

### Minor Changes

- reorder args of `get_gitops_ready` ([#33](https://github.com/ryanatkn/fuz_gitops/pull/33))
- sync git with remote in `load_local_repo` ([#33](https://github.com/ryanatkn/fuz_gitops/pull/33))
- add publishing support ([#36](https://github.com/ryanatkn/fuz_gitops/pull/36))
- update peer deps ([#36](https://github.com/ryanatkn/fuz_gitops/pull/36))

## 0.50.1

### Patch Changes

- fix two links in repos components ([62e09f6](https://github.com/ryanatkn/fuz_gitops/commit/62e09f6))

## 0.50.0

### Minor Changes

- upgrade deps ([#35](https://github.com/ryanatkn/fuz_gitops/pull/35))

## 0.49.0

### Minor Changes

- upgrade zod@4 ([#34](https://github.com/ryanatkn/fuz_gitops/pull/34))

## 0.48.0

### Minor Changes

- upgrade moss ([42e6ece](https://github.com/ryanatkn/fuz_gitops/commit/42e6ece))

## 0.47.0

### Minor Changes

- upgrade gro ([f7aadf0](https://github.com/ryanatkn/fuz_gitops/commit/f7aadf0))
- bump node@22.15 from 22.11 ([b7bc4af](https://github.com/ryanatkn/fuz_gitops/commit/b7bc4af))

## 0.46.0

### Minor Changes

- upgrade deps ([bc730f7](https://github.com/ryanatkn/fuz_gitops/commit/bc730f7))

## 0.45.2

### Patch Changes

- bump gro in https://github.com/ryanatkn/fuz_gitops/commit/36cc42838b3102b9967e7871ef717db80fe6d98b ([4554517](https://github.com/ryanatkn/fuz_gitops/commit/4554517))

## 0.45.1

### Patch Changes

- migrate to $app/state from $app/stores ([2f880dd](https://github.com/ryanatkn/fuz_gitops/commit/2f880dd))

## 0.45.0

### Minor Changes

- bump node@22.11 ([#32](https://github.com/ryanatkn/fuz_gitops/pull/32))

## 0.44.0

### Minor Changes

- run `gro sync` only if the branch changes ([1180456](https://github.com/ryanatkn/fuz_gitops/commit/1180456))

### Patch Changes

- sync at start of `gro gitops` with optional `--no-sync` arg ([3cf5142](https://github.com/ryanatkn/fuz_gitops/commit/3cf5142))

## 0.43.0

### Minor Changes

- rework local resolution and separate repo loading ([2476be1](https://github.com/ryanatkn/fuz_gitops/commit/2476be1))

## 0.42.0

### Minor Changes

- check current repo in `get_gitops_ready` instead of the task ([0d8cce5](https://github.com/ryanatkn/fuz_gitops/commit/0d8cce5))

## 0.41.1

### Patch Changes

- run `gro gen` after `gro gitops` if there are changes ([eb3ca42](https://github.com/ryanatkn/fuz_gitops/commit/eb3ca42))

## 0.41.0

### Minor Changes

- rename to `SECRET_GITHUB_API_TOKEN` from `GITHUB_TOKEN_SECRET` ([bea3232](https://github.com/ryanatkn/fuz_gitops/commit/bea3232))

## 0.40.1

### Patch Changes

- fix repo url to use ssh when cloning ([a7ceea8](https://github.com/ryanatkn/fuz_gitops/commit/a7ceea8))

## 0.40.0

### Minor Changes

- rename `SECRET_GITHUB_TOKEN` from `GITHUB_TOKEN_SECRET` ([fb306be](https://github.com/ryanatkn/fuz_gitops/commit/fb306be))

## 0.39.0

### Minor Changes

- add `gro gitops_ready` task ([#31](https://github.com/ryanatkn/fuz_gitops/pull/31))

## 0.38.0

### Minor Changes

- upgrade to use `create_context` ([deab96d](https://github.com/ryanatkn/fuz_gitops/commit/deab96d))

## 0.37.0

### Minor Changes

- bump required node version to `20.17` ([e114227](https://github.com/ryanatkn/fuz_gitops/commit/e114227))

## 0.36.0

### Minor Changes

- upgrade `@ryanatkn/fuz@0.119` from `0.118.2` and `@ryanatkn/moss@0.12` from `0.11.1` ([df573dc](https://github.com/ryanatkn/fuz_gitops/commit/df573dc))

## 0.35.0

### Minor Changes

- upgrade deps ([117e0f4](https://github.com/ryanatkn/fuz_gitops/commit/117e0f4))

## 0.34.0

### Minor Changes

- loosen peer deps temporarily ([a7dc4c7](https://github.com/ryanatkn/fuz_gitops/commit/a7dc4c7))

## 0.33.0

### Minor Changes

- change the gitops config source of truth from a deployment url to the repo url ([#30](https://github.com/ryanatkn/fuz_gitops/pull/30))

## 0.32.0

### Minor Changes

- pin peer deps ([#29](https://github.com/ryanatkn/fuz_gitops/pull/29))
- rename `Repo` from `Deployment` ([#29](https://github.com/ryanatkn/fuz_gitops/pull/29))

## 0.31.1

### Patch Changes

- format repos with multiline strings ([7799fc2](https://github.com/ryanatkn/fuz_gitops/commit/7799fc2))

## 0.31.0

### Minor Changes

- upgrade `@ryanatkn/fuz@0.110.4` from `0.108.4` ([acb7bb4](https://github.com/ryanatkn/fuz_gitops/commit/acb7bb4))

## 0.30.2

### Patch Changes

- add tsconfig `sourceRoot` ([b50c370](https://github.com/ryanatkn/fuz_gitops/commit/b50c370))
- publish src files ([68129cc](https://github.com/ryanatkn/fuz_gitops/commit/68129cc))
- enable tsconfig `declaration` and `declarationMap` ([936beb5](https://github.com/ryanatkn/fuz_gitops/commit/936beb5))

## 0.30.1

### Patch Changes

- improve `gro gitops` path handling and add a banner to the generated `repos.ts` ([f18720d](https://github.com/ryanatkn/fuz_gitops/commit/f18720d))

## 0.30.0

### Minor Changes

- change `$routes/repos.ts` from `$lib/deployments.json` and add `outdir` to `gro gitops` to customize it ([#28](https://github.com/ryanatkn/fuz_gitops/pull/28))

### Patch Changes

- add `sideEffects` to `package.json` ([c91c043](https://github.com/ryanatkn/fuz_gitops/commit/c91c043))

## 0.29.1

### Patch Changes

- upgrade gro with correctly formatted exports ([28379ed](https://github.com/ryanatkn/fuz_gitops/commit/28379ed))

## 0.29.0

### Minor Changes

- support `node@20.12` and later ([46b804b](https://github.com/ryanatkn/fuz_gitops/commit/46b804b))

## 0.28.0

### Minor Changes

- upgrade `node@22.3` and `@fuzdev/gro@0.120.0` ([eca969a](https://github.com/ryanatkn/fuz_gitops/commit/eca969a))

## 0.27.0

### Minor Changes

- throw on 401s in GitHub fetch helpers to abort on invalid tokens ([#24](https://github.com/ryanatkn/fuz_gitops/pull/24))

## 0.26.0

### Minor Changes

- set peer deps for `svelte` and `@sveltejs/kit` ([#23](https://github.com/ryanatkn/fuz_gitops/pull/23)) ([287df05](https://github.com/ryanatkn/fuz_gitops/commit/287df05))

## 0.25.0

### Minor Changes

- upgrade to svelte 5 ([#21](https://github.com/ryanatkn/fuz_gitops/pull/21))
- rename `ModulesNav` from `ModulesMenu` ([#21](https://github.com/ryanatkn/fuz_gitops/pull/21))

### Patch Changes

- add `PageHeader` and `PageFooter` ([#21](https://github.com/ryanatkn/fuz_gitops/pull/21))

## 0.24.0

### Minor Changes

- upgrade deps ([1e0ef29](https://github.com/ryanatkn/fuz_gitops/commit/1e0ef29))

## 0.23.1

### Patch Changes

- format URLs correctly with pathname ([89c315f](https://github.com/ryanatkn/fuz_gitops/commit/89c315f))

## 0.23.0

### Minor Changes

- upgrade `@ryanatkn/fuz@0.91.0` ([1a5a0f7](https://github.com/ryanatkn/fuz_gitops/commit/1a5a0f7))

## 0.22.0

### Minor Changes

- upgrade deps ([32be3d9](https://github.com/ryanatkn/fuz_gitops/commit/32be3d9))

## 0.21.0

### Minor Changes

- upgrade deps ([d6e8234](https://github.com/ryanatkn/fuz_gitops/commit/d6e8234))

## 0.20.1

### Patch Changes

- fix imports ([cf7aecd](https://github.com/ryanatkn/fuz_gitops/commit/cf7aecd))

## 0.20.0

### Minor Changes

- gitops.fuz.dev ([#20](https://github.com/ryanatkn/fuz_gitops/pull/20))

## 0.19.0

### Minor Changes

- upgrade ([205563c](https://github.com/ryanatkn/fuz_gitops/commit/205563c))

## 0.18.0

### Minor Changes

- republish ([9cc181e](https://github.com/ryanatkn/fuz_gitops/commit/9cc181e))

## 0.17.0

### Minor Changes

- upgrade @grogarden/util and switch to use its `fetch_value` ([#18](https://github.com/ryanatkn/fuz_gitops/pull/18))

## 0.16.3

### Patch Changes

- use `selected` instead of `active` for link classes ([1cdc18f](https://github.com/ryanatkn/fuz_gitops/commit/1cdc18f))

## 0.16.2

### Patch Changes

- fix `parse_deployments` to require a `homepage_url` ([d1bccb4](https://github.com/ryanatkn/fuz_gitops/commit/d1bccb4))

## 0.16.1

### Patch Changes

- fix tree nav flex direction ([a1cc247](https://github.com/ryanatkn/fuz_gitops/commit/a1cc247))

## 0.16.0

### Minor Changes

- fix local package `gro gitops` ([#15](https://github.com/ryanatkn/fuz_gitops/pull/15))

### Patch Changes

- add `PageFooter`, `PageHeader`, and page components ([#16](https://github.com/ryanatkn/fuz_gitops/pull/16))
- add tree nav component ([#12](https://github.com/ryanatkn/fuz_gitops/pull/12))

## 0.15.0

### Minor Changes

- query and display CI status ([#17](https://github.com/ryanatkn/fuz_gitops/pull/17))

### Patch Changes

- fix pull request links ([523131f](https://github.com/ryanatkn/fuz_gitops/commit/523131f))

## 0.14.0

### Minor Changes

- rename `DeploymentsTable` from `RepoTable` ([70d4b0d](https://github.com/ryanatkn/fuz_gitops/commit/70d4b0d))

## 0.13.3

### Patch Changes

- make DeploymentsTree full the available width ([19809e1](https://github.com/ryanatkn/fuz_gitops/commit/19809e1))

## 0.13.2

### Patch Changes

- fix local package ([fb802a2](https://github.com/ryanatkn/fuz_gitops/commit/fb802a2))

## 0.13.1

### Patch Changes

- fix deployments.json type ([3bb681a](https://github.com/ryanatkn/fuz_gitops/commit/3bb681a))

## 0.13.0

### Minor Changes

- upgrade gro with src_json ([#13](https://github.com/ryanatkn/fuz_gitops/pull/13))

## 0.12.0

### Minor Changes

- upgrade deps ([b25acb6](https://github.com/ryanatkn/fuz_gitops/commit/b25acb6))

## 0.11.0

### Minor Changes

- rename `ModulesDetail` slot `"nav"` from `"menu"` ([#11](https://github.com/ryanatkn/fuz_gitops/pull/11))

### Patch Changes

- add `DeploymentsTree` ([#11](https://github.com/ryanatkn/fuz_gitops/pull/11))
- add breadcrumb to `ModulesDetail` ([6088428](https://github.com/ryanatkn/fuz_gitops/commit/6088428))

## 0.10.9

### Patch Changes

- fix whitespace ([7de4255](https://github.com/ryanatkn/fuz_gitops/commit/7de4255))

## 0.10.8

### Patch Changes

- add `gitops.task.ts` ([1e2dc8f](https://github.com/ryanatkn/fuz_gitops/commit/1e2dc8f))
- use `if-modified-since` and `last-modified` headers ([68e4f47](https://github.com/ryanatkn/fuz_gitops/commit/68e4f47))

## 0.10.7

### Patch Changes

- improve `PullRequestsDetail` ([9887469](https://github.com/ryanatkn/fuz_gitops/commit/9887469))

## 0.10.6

### Patch Changes

- add `PullRequestsDetail` ([#10](https://github.com/ryanatkn/fuz_gitops/pull/10))

## 0.10.5

### Patch Changes

- fix `ModulesDetail` layout ([c6d69c3](https://github.com/ryanatkn/fuz_gitops/commit/c6d69c3))

## 0.10.4

### Patch Changes

- improve `ModulesDetail` ([4e379a1](https://github.com/ryanatkn/fuz_gitops/commit/4e379a1))

## 0.10.3

### Patch Changes

- fix a link ([00b94f0](https://github.com/ryanatkn/fuz_gitops/commit/00b94f0))

## 0.10.2

### Patch Changes

- show only published deployments ([e61c437](https://github.com/ryanatkn/fuz_gitops/commit/e61c437))

## 0.10.1

### Patch Changes

- rearrange repo table columns ([6041e50](https://github.com/ryanatkn/fuz_gitops/commit/6041e50))

## 0.10.0

### Minor Changes

- upgrade deps ([b6b7f7b](https://github.com/ryanatkn/fuz_gitops/commit/b6b7f7b))

## 0.9.0

### Minor Changes

- upgrade deps ([#9](https://github.com/ryanatkn/fuz_gitops/pull/9))

## 0.8.2

### Patch Changes

- publish sample data ([#8](https://github.com/ryanatkn/fuz_gitops/pull/8))
- add `ModulesDetail.svelte` and `ModulesNav.svelte` ([#8](https://github.com/ryanatkn/fuz_gitops/pull/8))

## 0.8.1

### Patch Changes

- add `package.ts` ([7e888d3](https://github.com/ryanatkn/fuz_gitops/commit/7e888d3))

## 0.8.0

### Minor Changes

- upgrade deps ([2c0164c](https://github.com/ryanatkn/fuz_gitops/commit/2c0164c))

## 0.7.1

### Patch Changes

- log package cache status ([fe97baf](https://github.com/ryanatkn/fuz_gitops/commit/fe97baf))

## 0.7.0

### Minor Changes

- add cache for `gro gitops` ([#7](https://github.com/ryanatkn/fuz_gitops/pull/7))
- snake_case everywhere ([#7](https://github.com/ryanatkn/fuz_gitops/pull/7))

## 0.6.2

### Patch Changes

- add favicon to `DeploymentsTable` ([9c0e326](https://github.com/ryanatkn/fuz_gitops/commit/9c0e326))

## 0.6.1

### Patch Changes

- make `GithubPullRequest` `body` nullable ([77f0ad6](https://github.com/ryanatkn/fuz_gitops/commit/77f0ad6))

## 0.6.0

### Minor Changes

- extract `$lib/github.ts` ([#6](https://github.com/ryanatkn/fuz_gitops/pull/6))
- add `GithubPullRequest` schema and parse fetch response ([#6](https://github.com/ryanatkn/fuz_gitops/pull/6))

## 0.5.2

### Patch Changes

- fix package fetching error handling ([b7cb0df](https://github.com/ryanatkn/fuz_gitops/commit/b7cb0df))

## 0.5.1

### Patch Changes

- add peer dep for @octokit/request ([8dbc32a](https://github.com/ryanatkn/fuz_gitops/commit/8dbc32a))

## 0.5.0

### Minor Changes

- support `pull_requests` for public repos ([#5](https://github.com/ryanatkn/fuz_gitops/pull/5))

## 0.4.0

### Minor Changes

- parse `GitopsConfig` ([1ed8dc4](https://github.com/ryanatkn/fuz_gitops/commit/1ed8dc4))

### Patch Changes

- strict config ([1ed8dc4](https://github.com/ryanatkn/fuz_gitops/commit/1ed8dc4))

## 0.3.0

### Minor Changes

- rename `GitopsConfig` `deployments` from `repos` ([0e16e6f](https://github.com/ryanatkn/fuz_gitops/commit/0e16e6f))

## 0.2.1

### Patch Changes

- fix generated type file path ([49f61b1](https://github.com/ryanatkn/fuz_gitops/commit/49f61b1))

## 0.2.0

### Minor Changes

- update exports ([fb9de5d](https://github.com/ryanatkn/fuz_gitops/commit/fb9de5d))

## 0.1.1

### Patch Changes

- upgrade gro to fix default svelte exports ([33f0f77](https://github.com/ryanatkn/fuz_gitops/commit/33f0f77))

## 0.1.0

### Minor Changes

- init ([408f471](https://github.com/ryanatkn/fuz_gitops/commit/408f471))
