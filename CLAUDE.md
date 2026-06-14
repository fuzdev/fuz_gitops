# fuz_gitops

> Multi-repo management - alternative to monorepo pattern

fuz_gitops (`@fuzdev/fuz_gitops`) loosely couples repos with cascade publishing
and cross-repo automation.

For coding conventions, see Skill(fuz-stack).

## Committing

`git add` and `git commit` are denied by `.claude/settings.local.json` in
this repo — make the edits and stop, the user commits.

## Table of Contents

- [Core functionality](#core-functionality)
- [Architecture](#architecture)
- [Patterns](#patterns)
- [Configuration](#configuration)
- [Main operations](#main-operations)
- [Data types](#data-types)
- [UI components](#ui-components)
- [Commands](#commands)
- [Dependencies](#dependencies)
- [General Patterns](#general-patterns)
- [Testability & Operations Pattern](#testability--operations-pattern)
- [Testing](#testing)
- [Generated Files & Caches](#generated-files--caches)
- [Additional Documentation](#additional-documentation)

## Core functionality

- Fetches metadata from repo collections via GitHub API
- Manages local repo clones and syncs branches
- Generates typesafe JSON from package.json and exported modules metadata
- Publishes docs websites for repo collections
- Tracks CI status and pull requests

## Architecture

```
gitops.config.ts -> local repos -> GitHub API -> repos.ts -> UI components
```

### Key files

- `gitops.config.ts` - user config defining repo collections
- `src/lib/gitops_sync.task.ts` - syncs local repos and generates UI data
- `src/lib/gitops_analyze.task.ts` - analyzes dependencies and changesets
- `src/lib/gitops_plan.task.ts` - generates publishing plan
- `src/lib/gitops_publish.task.ts` - publishes repos in dependency order
- `src/lib/gitops_validate.task.ts` - runs all validation checks
- `src/lib/local_repo.ts` - manages local repo clones, branch switching
- `src/lib/github.ts` - GitHub API client for PRs, CI status
- `src/lib/fetch_repo_data.ts` - fetches remote repo metadata
- `src/routes/repos.ts` - generated data file with all repo info

## Patterns

### Plan-Driven Publishing

Publishing has two stages with the plan as the single source of truth:

- **Plan** (`generate_publishing_plan`) resolves the full cascade up front using
  fixed-point iteration (max 10 iterations): explicit changesets, bump
  escalations from breaking dependencies, and auto-generated changesets for
  dependents. It converges when no new version changes are discovered, and warns
  with a pending-package count if it hits the iteration limit.
- **Publish** (`publish_repos`) executes the frozen plan in a single linear pass
  over the topological order — it re-derives nothing. Publishing a package
  immediately rewrites each dependent's `package.json` and creates its
  auto-changeset, so by the time the pass reaches a package its changeset
  already exists. A single pass converges by construction; there is no
  publish-side loop. The dry run reports the same plan; a single
  `gro gitops_publish --wetrun` handles the full cascade.
- **Fail loud on drift**: if a real publish lands a version the plan did not
  predict, publishing aborts (an invariant violation, surfaced as a `drift`
  failure) rather than silently re-deriving — see Dirty State on Failure below.

The dependency-driven bump rule (pre-1.0 → minor for a breaking dep, else patch;
1.0+ → major or patch) lives once in `required_bump_for_dependency_update`
(`version_utils.ts`), shared by the plan and the auto-changeset generator so the
two never disagree.

### Dirty State on Failure (By Design)

Publishing intentionally leaves the workspace dirty when failures occur:

- Auto-changesets are created and committed DURING the publishing pass
- If publishing fails mid-way — a publish error, an npm-propagation timeout, or
  a plan/reality drift — some packages are published, others are not
- The dirty workspace state shows exactly what succeeded/failed
- This enables **natural resumption**: just fix the issue and re-run the same
  command, which re-plans from the current state
- Already-published packages have no changesets → drop out of the new plan
- Failed packages still have changesets → retried automatically

### No Rollback Support

fuz_gitops does not support rollback of published packages:

- NPM does not support reliable unpublishing of packages
- Once a package is published to NPM, it cannot be easily reverted
- If publishing fails, you must publish forward (fix the issue and continue)
- The dirty workspace state shows exactly which packages succeeded

### No Concurrent Publishing

This tool is not designed for concurrent use. Running multiple
`gro gitops_publish` commands simultaneously is not supported and will cause
conflicts on git commits and changeset files.

## Configuration

```ts
// gitops.config.ts
export default {
	repos: [
		'https://github.com/owner/repo',
		{
			repo_url: '...',
			repo_dir: '...',
			branch: 'main',
		},
	],
};
```

Requires `SECRET_GITHUB_API_TOKEN` in `.env` for API access.

## Main operations

### `gro gitops_sync` Task

1. Loads config from `gitops.config.ts`
2. Resolves local repos (clones missing if `--download`)
3. Switches branches and syncs as needed
4. Fetches GitHub data (CI, PRs)
5. Generates `src/routes/repos.ts`
6. Updates cache

### Local repo management

Branch switching, pulling, and installing happen only on the **sync path** —
`gro gitops_sync` and any diagnostic run with `--sync`. By default the
diagnostics load repos as-is via `get_gitops_ready({sync: false})` and skip all
of the below. The shared `get_gitops_ready` helper (`gitops_task_helpers.ts`)
gates this with its `sync` option, threaded down to `local_repo_load`.

- Resolves repo URLs to local directories
- Clones missing repos via SSH
- Switches branches maintaining clean workspace (`--allow-dirty` to tolerate a dirty tree)
- Automatically installs dependencies when package.json changes:
  - After initial clone
  - After pulling latest changes
  - After switching branches (if package.json differs)
  - Uses `npm install` to ensure dependencies match package.json

### Data fetching

- Pull requests via GitHub API
- CI check runs and status
- Package metadata from .well-known endpoints
- Caches responses to minimize API calls

### Multi-repo publishing

#### Publishing Workflow

- `gro gitops_publish --wetrun` - publishes repos in dependency order
  - Executes the precomputed plan in a single linear pass (no publish-side loop)
  - Creates auto-changesets for dependent packages during the pass
  - Fails loud and aborts if a publish drifts from the plan's prediction
- `gro gitops_plan` - generates a publishing plan (read-only prediction)
- `gro gitops_analyze` - analyzes dependencies and changesets
- `gro gitops_publish` - previews publishing (dry run) without preflight checks
  or state persistence; reports the same full cascade as `gro gitops_plan`
- Handles circular dev dependencies by excluding from topological sort
- Waits for NPM propagation with exponential backoff (10 minute default
  timeout):
  - NPM uses eventually consistent CDN distribution
  - Published packages may not be immediately available globally
  - Critical for multi-repo: ensures dependencies are fetchable before
    publishing dependents
- Updates cross-repo dependencies automatically
- Preflight checks validate clean workspaces, branches, builds, and npm
  authentication (skipped for dry runs)

**Build Validation (Fail-Fast Safety)**

The publishing workflow includes build validation in preflight checks to prevent
broken state:

1. **Preflight phase** (before any publishing):
   - Runs `gro build` on all packages with changesets
   - This is a **builds-today smoke test** against the current, pre-cascade
     dependency versions — it catches a repo that won't build at all before the
     run starts touching npm, but it cannot validate a package against the
     versions about to be published (those don't exist yet)
   - Fails fast if ANY build fails

2. **Publishing phase** (after validation):
   - Runs `gro publish --no-build` for each package
   - `gro publish` still runs `gro check` internally (typecheck, test, lint) —
     and because the dependent's `package.json` is rewritten before this step and
     `gro publish` reinstalls (ETARGET-healing) internally, that check is the real
     validation against the **just-published** dependency versions. `--no-build`
     is safe because every
     publishable package is a `svelte-package` library shipping unbundled `dist`:
     a dependency version change never alters the dependent's `dist` bytes, so the
     preflight-validated build stays valid
   - Optionally deploys repos with changes if `--deploy` flag used (published or
     any dep updates). Deploys build fresh (the deploy step does not pass
     `--no-build`) so a deployed site reflects the versions just published — the
     preflight build ran against the old versions, before the cascade.

This prevents the known issue in `gro publish` where build failures leave repos
in broken state (version bumped but not published).

**Dependency Installation (delegated to gro)**

The publishing executor never runs a bare `npm install` itself. Installing
dependencies is gro's responsibility, and gro's install path self-heals npm's
stale-cache (ETARGET) failure mode — clear the cache and retry once when a
just-published version isn't visible yet. So fuz_gitops carries no install or
cache-healing logic of its own:

1. **Republishing dependents:** after a package publishes, the executor rewrites
   its prod/peer dependents' `package.json` ranges and commits them. When the
   pass reaches a dependent and runs `gro publish`, gro installs the rewritten
   deps (ETARGET-healing if npm hasn't caught up) as part of publishing it.
2. **Dev-dep-only dependents:** these never run `gro publish`. The executor
   bumps + commits their `package.json` but does **not** install them; their
   `node_modules` is refreshed (and ETARGET-healed) by gro the next time they
   build, deploy (`gro deploy` builds fresh), or sync (`gro gitops_sync`).

This is why `gro publish --no-build` is safe immediately after a publish: its
internal install heals the cache. There is no `--skip-install` flag — there are
no executor-owned installs to skip.

**Plan vs Dry Run**

`gro gitops_plan`:

- **Read-only prediction** - Generates a publishing plan showing what would be
  published
- Uses fixed-point iteration to resolve transitive cascades (max 10 iterations)
- Shows all 4 publishing scenarios: explicit changesets, bump escalation,
  auto-generated changesets, and no changes
- No side effects - does not modify any files or state

`gro gitops_publish` (dry run, default):

- **Plan-driven preview** - The dry run consumes the same plan as `gro
gitops_plan` and reports the full cascade (explicit changesets, bump
  escalations, and auto-generated changesets)
- Skips preflight checks (workspace, branch, npm auth)
- No side effects - reports what `--wetrun` would publish; the count matches
  `gro gitops_plan` (the plan is the single source of truth for the cascade)

#### Changeset Semantics

Four publishing scenarios (see ./docs/publishing.md for
details):

1. **Explicit changesets** - Normal publishing with version bump from changesets
2. **Bump escalation** - Changeset bump overridden by dependency requirements
3. **Auto-generated** - No changesets but prod/peer deps updated
4. **No changes** - Skipped (normal behavior)

**Dependency behavior**: Production/peer deps trigger republish; dev deps only
update package.json without republishing.

#### Private Packages

Packages with `"private": true` never publish. They are excluded from the plan's
version changes — no publish, npm-wait, bump escalation, or auto-changeset — so
the executor skips them even though they keep their slot in the topological
publishing order. A private package that depends on a published one is handled as
an **update-only leaf**: its dependency ranges are rewritten and committed
*without* a changeset (it won't republish). A private package carrying its own
changeset is flagged in the plan's warnings, since that changeset can't be
published.

#### Key Publishing Modules

- `multi_repo_publisher.ts` - Main publishing orchestration (`generate_publishing_plan`
  builds the plan, `execute_publishing_plan` executes the frozen plan; `publish_repos`
  composes the two)
- `publishing_plan.ts` - Publishing plan generation and cascade analysis
- `publish_steps.ts` - Derives the ordered side-effect preview (`--preview`) from a plan
- `changeset_reader.ts` - Parses changesets and predicts versions
- `changeset_generator.ts` - Auto-generates changesets for dependency updates
- `dependency_graph.ts` - Topological sorting and cycle detection
- `graph_validation.ts` - Shared cycle detection and publishing order
  computation
- `version_utils.ts` - Version comparison and bump type detection
- `npm_registry.ts` - NPM availability checks with retry
- `dependency_updater.ts` - Package.json updates with changesets
- `preflight_checks.ts` - Pre-publish validation including build checks
- `operations.ts` - Dependency injection interfaces for testability (including
  build operations)

#### Publishing Algorithms

See ./docs/publishing.md for detailed algorithm
descriptions.

**Fixed-Point Iteration**: Plan generation uses iterative passes (max 10) to
resolve transitive cascades, identifying packages needing publish due to
dependency updates until no new changes are discovered. The publisher then
executes that frozen plan in a single pass — the iteration is in planning, not
publishing.

**Cycle Detection**: Production/peer cycles block publishing (error). Dev cycles
allowed (warning only, excluded from topological sort). Publishing order
computed via topological sort on prod/peer deps only.

## Data types

```ts
class Repo {
	readonly library: Library;
	check_runs: GithubCheckRunsItem | null;
	pull_requests: Array<GithubPullRequest> | null;
}

interface LocalRepo {
	library: Library;
	package_json: PackageJson;
	repo_dir: string;
	repo_git_ssh_url: string;
	repo_config: GitopsRepoConfig;
	dependencies?: Map<string, string>;
	dev_dependencies?: Map<string, string>;
	peer_dependencies?: Map<string, string>;
}

interface LocalRepoPath {
	type: 'local_repo_path';
	repo_name: string;
	repo_dir: string;
	repo_url: string;
}
```

## UI components

- `ReposTable.svelte` - dependency matrix view
- `ReposTree.svelte` - hierarchical repo browser
- `Modules_*.svelte` - module exploration
- `Pull_Requests_*.svelte` - PR tracking

## Commands

```bash
npm i -D @fuzdev/fuz_gitops

# Data management
gro gitops_sync               # sync repos and update local data
gro gitops_sync --download    # clone missing repos
gro gitops_sync --check       # verify repos are ready without fetching data
gro gitops_sync --allow-dirty # sync (switch branch, pull) tolerating uncommitted changes

# Run commands across repos (reads repos as-is, no branch switch/pull)
gro gitops_run "npm test"                          # run command in all repos (parallel, concurrency: 5)
gro gitops_run "npm audit" --concurrency 3         # limit parallelism
gro gitops_run "gro check" --format json           # JSON output (logged to stdout)
gro gitops_run "gro check" --format json --outfile out.json # clean JSON to a file

# Publishing
gro gitops_validate              # validate configuration (runs analyze, plan, dry run, and ci_reconcile)
gro gitops_analyze               # analyze dependencies and changesets
gro gitops_plan                  # generate publishing plan
gro gitops_plan --verbose        # show additional details
gro gitops_plan --sync           # switch branch + pull + install before planning
gro gitops_publish               # dry run (default, simulates publishing)
gro gitops_publish --wetrun      # actually publish repos in dependency order
gro gitops_publish --wetrun --no-plan # skip interactive plan confirmation
gro gitops_publish --verbose     # show additional details in plan
gro gitops_publish --preview     # print the ordered side-effects a --wetrun would perform
gro gitops_publish --emit-json   # stream structured publishing events as JSON-lines to stdout

# Output formats (analyze, plan, publish)
gro gitops_analyze --format json --outfile analysis.json
gro gitops_plan --format markdown --outfile plan.md

# Development
gro dev        # start dev server
gro build      # build static site
gro deploy     # deploy to GitHub Pages

# Fixture Management
gro src/test/fixtures/generate_repos # generate test git repos from fixture data
gro test src/test/fixtures/check     # validate gitops commands against fixture expectations
```

### Commands by Side Effects

**Read-Only (Safe, No Side Effects):**

These read each repo's working tree **as-is** by default — no branch switch,
pull, install, or clean-workspace check — so they're safe on an active
workspace with feature branches and uncommitted changes. Pass `--sync` to
refresh repos (switch to the configured branch, pull, install) first.

- `gro gitops_analyze` - Analyze dependency graph, detect cycles
- `gro gitops_plan` - Generate publishing plan showing version changes and
  cascades
- `gro gitops_validate` - Run all validation checks (analyze + plan + dry run)
- `gro gitops_publish` - Simulate publishing without preflight checks (dry run default)

**Data Sync (Local Changes Only):**

- `gro gitops_sync` - Fetch repo metadata, generate src/routes/repos.ts
  - Clones missing repos (with `--download`)
  - Switches branches and pulls latest changes
  - Installs dependencies if package.json changed
  - Verify repos ready without fetching (with `--check`)
  - Tolerate uncommitted changes when syncing (with `--allow-dirty`)
  - Runs in parallel (concurrency: 5 by default)

**Command Execution (User-Defined Side Effects):**

- `gro gitops_run "<command>"` - Run shell command across all repos
  - Parallel execution (concurrency: 5 by default)
  - Continue-on-error behavior
  - Structured output (text or JSON)
  - Use for testing, auditing, batch operations

**Publishing (Git & NPM Side Effects):**

- `gro gitops_publish --wetrun` - Publish packages, update dependencies, git commits

### Command Workflow

- `gitops_validate` runs: `gitops_analyze` + `gitops_plan` +
  `gitops_publish` (dry run) + `ci_reconcile`. It hard-fails (throws) on any
  error from any step — a production dependency cycle, a plan error, or CI
  drift — so a clear problem stops the run. Warnings stay non-fatal.
- `gitops_publish --wetrun` runs: `gitops_plan` (with confirmation) + actual publish

## Dependencies

- `@fuzdev/gro` - build tool and task runner
- `@fuzdev/fuz_ui` - UI components and utilities
- `@fuzdev/fuz_util` - utility functions
- `@fuzdev/fuz_css` - CSS framework and design tokens
- `@sveltejs/kit` - web framework
- `svelte` - UI framework
- `zod` - schema validation

## General Patterns

- Uses Gro's well-known package.json patterns for metadata
- Generates static JSON for fast client-side rendering
- Caches API responses to minimize API calls
- Atomic file updates with format checking
- Supports both relative and absolute repo paths
- Functional programming patterns (arrow functions, pure functions)
- Changeset-driven versioning with auto-generation
- Natural resumption via changeset consumption (no state files needed)

### Peer Dependency Versioning Strategy

For packages you control, use `>=` instead of `^` for peer dependencies:

```json
"peerDependencies": {
  "@fuzdev/fuz_util": ">=0.38.0", // controlled package - use >=
  "@fuzdev/gro": ">=0.174.0",   // controlled package - use >=
  "@sveltejs/kit": "^2",          // third-party - use ^
  "svelte": "^5"                  // third-party - use ^
}
```

**Why `>=` for controlled packages:**

- Eliminates npm peer dependency resolution conflicts when publishing sequentially
- `^0.37.0` means `>=0.37.0 <0.38.0` in 0.x semver (excludes next minor)
- When you publish `fuz_css@0.38.0`, packages with `"@fuzdev/fuz_css": "^0.37.0"`
  conflict
- `>=0.37.0` allows any version `>=0.37.0`, including `0.38.0` and beyond
- No need for `--legacy-peer-deps` flag

**Why `^` for third-party packages:**

- You don't control when they make breaking changes
- `^` protects users from accidental incompatibility

**Version prefix preservation:**

When fuz_gitops updates dependencies, it preserves existing prefixes:

- `>=0.38.0` updates to `>=0.39.0` (preserves `>=`)
- `^1.0.0` updates to `^1.1.0` (preserves `^`)
- `~1.0.0` updates to `~1.1.0` (preserves `~`)

## Testability & Operations Pattern

This project uses **dependency injection** for all side effects, making it fully
testable without mocks:

**Why:** Functions that call git, npm, or file system are hard to test. The
operations pattern abstracts these into interfaces.

**How:** See `src/lib/operations.ts` - all external dependencies (git, npm, fs,
process, build) are defined as interfaces. Tests provide mock implementations.

**Benefits:**

- **No mocking libraries** - Just plain objects implementing interfaces
- **Type-safe tests** - Mock implementations must match interface signatures
- **Easy setup** - Return exactly what you want from fake operations
- **Fast tests** - No real git/npm/fs operations, instant execution
- **Predictable** - Control all side effects explicitly
- **Readable** - Test code shows exactly what operations do

**Example:**

- Production: `multi_repo_publisher(repos, options, default_gitops_operations)`
- Tests: `multi_repo_publisher(repos, options, mock_gitops_operations)`

See `src/lib/operations_defaults.ts` for real implementations and test files for
mock implementations.

**When writing new code:**

- Add side effects as operations interface methods (see `operations.ts`)
- Accept operations parameter with default:
  `ops: GitopsOperations = default_gitops_operations`
- Call operations through the injected parameter: `await ops.git.commit(...)`
- Tests inject fake operations that return controlled data

## Testing

Uses vitest with **zero mocks** - all tests use the operations pattern for
dependency injection (see above).

```bash
gro test                         # run all tests
gro test version_utils           # run specific test file
gro test src/test/fixtures/check # validate command output fixtures
```

Core modules tested:

- `version_utils.test.ts` - Version comparison and semver logic
- `changeset_reader.test.ts` - Changeset parsing and version prediction
- `dependency_graph.test.ts` - Topological sorting and cycle detection
- `changeset_generator.test.ts` - Auto-changeset content generation
- `preflight_checks.test.ts` - Workspace, branch, and npm validation
- `dependency_updater.test.ts` - Package.json updates and git commits

### Fixture Testing

The fixture system uses **generated git repositories** for isolated,
reproducible integration tests:

**Generated Test Repos:**

- `src/test/fixtures/repos/` - Auto-generated from fixture data (gitignored)
- `src/test/fixtures/repo_fixtures/*.ts` - Source of truth for test repo definitions
- `src/test/fixtures/generate_repos.ts` - Idempotent repo generation logic
- `src/test/fixtures/configs/*.config.ts` - Isolated gitops config per fixture

**Fixture Scenarios (10 total):**

- `basic_publishing` - All 4 publishing scenarios (explicit, auto-generated,
  bump escalation, no changes)
- `deep_cascade` - 4-level dependency chains with cascading breaking changes
- `circular_dev_deps` - Dev dependency cycles (allowed, non-blocking)
- `circular_prod_deps_error` - Production circular dependencies (error
  detection)
- `private_packages` - Private package handling (skipped from publishing)
- `major_bumps` - Major version transitions (0.x → 1.0, 1.x → 2.0)
- `peer_deps_only` - Plugin/adapter patterns (peer dependencies only)
- `isolated_packages` - Independent packages with no internal dependencies
- `multiple_dep_types` - Packages with both peer and dev deps on same dependency
- `three_way_dev_cycle` - Complex dev dependency cycles with three packages

**Structured Validation:**

- `src/test/fixtures/configs/*.config.ts` - Isolated gitops config per fixture
- `src/test/fixtures/check.test.ts` - Validates JSON output against fixture
  `expected_outcomes`
- `src/test/fixtures/helpers.ts` - JSON command runner and assertion helpers

**Workflow:**

1. Define fixture data with expected outcomes in `repo_fixtures/*.ts`
2. Run `gro test src/test/fixtures/check` to validate commands against expected
   outcomes

Fixture repos are auto-generated on first test run if missing. To manually
regenerate: `gro src/test/fixtures/generate_repos`

Each fixture runs in isolation with its own config, validating:

- Publishing order (topological sort correctness)
- Version changes (explicit, auto-generated, bump escalation scenarios)
- Breaking change cascades
- Warnings, errors, and info messages

Test repos are isolated from real workspace repos and can run in CI without
cloning.

## Generated Files & Caches

- **Repo data** — `gro gitops_sync` writes `repos.json` + `repos.ts` to the
  SvelteKit routes dir (`src/routes/` by default, overridable with `--outdir`).
  These are committed (the site renders from them).
- **Caches** (gitignored, under `.gro/`) — the fetch-value cache at
  `.gro/build/fetch/` and the `svelte-docinfo` library metadata at
  `.gro/library.json`.

## Additional Documentation

- [Publishing Guide](docs/publishing.md) - Workflows, changeset semantics,
  examples
- [Troubleshooting](docs/troubleshooting.md) - Common errors and debugging tips
