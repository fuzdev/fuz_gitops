# fuz_gitops

> Multi-repo management - alternative to monorepo pattern

fuz_gitops (`@fuzdev/fuz_gitops`) loosely couples repos with cascade publishing
and cross-repo automation.

For coding conventions, see [`fuz-stack`](../fuz-stack/CLAUDE.md).

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
- [Output Directory](#output-directory)
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

### Fixed-Point Iteration

Publishing uses fixed-point iteration to handle transitive dependency updates:

- Runs multiple passes (max 10 iterations) until convergence
- Each pass publishes packages with changesets AND creates auto-changesets for
  dependents mid-iteration
- Auto-changesets trigger republishing in subsequent passes (transitive
  cascades)
- Stops when no new changesets are created (converged state)
- Single `gro gitops_publish` command handles full dependency cascades
- If MAX_ITERATIONS reached without convergence, warns with pending package
  count and estimated iterations needed

### Dirty State on Failure (By Design)

Publishing intentionally leaves the workspace dirty when failures occur:

- Auto-changesets are created and committed DURING the publishing loop
- If publishing fails mid-way, some packages are published, others are not
- The dirty workspace state shows exactly what succeeded/failed
- This enables **natural resumption**: just fix the issue and re-run the same
  command
- Already-published packages have no changesets → skipped automatically
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

- Resolves repo URLs to local directories
- Clones missing repos via SSH
- Switches branches maintaining clean workspace
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

- `gro gitops_publish` - publishes repos in dependency order
  - Uses fixed-point iteration to handle transitive dependency updates
  - Converges after multiple passes (max 10 iterations)
  - Creates auto-changesets for dependent packages during publishing
- `gro gitops_plan` - generates a publishing plan (read-only prediction)
- `gro gitops_analyze` - analyzes dependencies and changesets
- `gro gitops_publish --dry_run` - simulates publishing without preflight checks
  or state persistence
- Handles circular dev dependencies by excluding from topological sort
- Waits for NPM propagation with exponential backoff (10 minute default
  timeout):
  - NPM uses eventually consistent CDN distribution
  - Published packages may not be immediately available globally
  - Critical for multi-repo: ensures dependencies are fetchable before
    publishing dependents
- Updates cross-repo dependencies automatically
- Preflight checks validate clean workspaces, branches, builds, and npm
  authentication (skipped for --dry_run runs)

**Build Validation (Fail-Fast Safety)**

The publishing workflow includes build validation in preflight checks to prevent
broken state:

1. **Preflight phase** (before any publishing):
   - Runs `gro build` on all packages with changesets
   - Validates builds using current versions (no side effects)
   - Fails fast if ANY build fails

2. **Publishing phase** (after validation):
   - Runs `gro publish --no-build` for each package
   - Builds already validated, so no risk of build failures mid-publish
   - Optionally deploys repos with changes if `--deploy` flag used (published or
     any dep updates)

This prevents the known issue in `gro publish` where build failures leave repos
in broken state (version bumped but not published).

**Dependency Installation with Cache Healing**

The publishing workflow automatically installs dependencies after package.json updates with smart cache healing:

1. **When installations happen:**
   - After each iteration when dependency updates occur
   - Batch installs all repos with updated dependencies before next iteration
   - Published packages skip install (`gro publish` handles it internally)

2. **Cache healing strategy:**
   - First attempt: regular `npm install`
   - On ETARGET error (package not found due to stale cache): `npm cache clean --force` then retry
   - On other errors: fail immediately without cache cleaning
   - Detects variations: "ETARGET", "notarget", "No matching version found"

3. **Why cache healing is needed:**
   - After publishing and waiting for NPM propagation, npm's local cache may still have stale "404" metadata
   - Cache clean forces fresh metadata fetch from registry
   - Ensures newly published packages can be installed by dependents

4. **Configuration:**
   - `--skip-install`: Disable installs during publishing (for speed/testing)
   - Installs enabled by default

**Plan vs Dry Run**

`gro gitops_plan`:

- **Read-only prediction** - Generates a publishing plan showing what would be
  published
- Uses fixed-point iteration to resolve transitive cascades (max 10 iterations)
- Shows all 4 publishing scenarios: explicit changesets, bump escalation,
  auto-generated changesets, and no changes
- No side effects - does not modify any files or state

`gro gitops_publish --dry_run`:

- **Simulated execution** - Runs the same code path as real publishing
- Skips preflight checks (workspace, branch, npm auth)
- Only simulates packages with explicit changesets (can't auto-generate
  changesets without real publishes)
- Use plan for comprehensive "what would happen" analysis; use dry run to test
  execution flow

#### Changeset Semantics

Four publishing scenarios (see [docs/publishing.md](docs/publishing.md) for
details):

1. **Explicit changesets** - Normal publishing with version bump from changesets
2. **Bump escalation** - Changeset bump overridden by dependency requirements
3. **Auto-generated** - No changesets but prod/peer deps updated
4. **No changes** - Skipped (normal behavior)

**Dependency behavior**: Production/peer deps trigger republish; dev deps only
update package.json without republishing.

#### Private Packages

Packages with `"private": true` are excluded from publishing order.

#### Key Publishing Modules

- `multi_repo_publisher.ts` - Main publishing orchestration
- `publishing_plan.ts` - Publishing plan generation and cascade analysis
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

See [docs/publishing.md](docs/publishing.md) for detailed algorithm
descriptions.

**Fixed-Point Iteration**: Publishing uses iterative passes (max 10) to resolve
transitive cascades. Each pass identifies new packages needing publish due to
dependency updates. Converges when no new changes discovered.

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
	library_json: LibraryJson;
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

# Run commands across repos
gro gitops_run "npm test"                   # run command in all repos (parallel, concurrency: 5)
gro gitops_run "npm audit" --concurrency 3  # limit parallelism
gro gitops_run "gro check" --format json    # JSON output for scripting

# Publishing
gro gitops_validate          # validate configuration (runs analyze, plan, and dry run)
gro gitops_analyze           # analyze dependencies and changesets
gro gitops_plan              # generate publishing plan
gro gitops_plan --verbose    # show additional details
gro gitops_publish           # publish repos in dependency order (interactive y/n prompt)
gro gitops_publish --dry_run # dry run without preflight checks
gro gitops_publish --no-plan # skip interactive plan confirmation
gro gitops_publish --verbose # show additional details in plan

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

- `gro gitops_analyze` - Analyze dependency graph, detect cycles
- `gro gitops_plan` - Generate publishing plan showing version changes and
  cascades
- `gro gitops_validate` - Run all validation checks (analyze + plan + dry run)
- `gro gitops_publish --dry_run` - Simulate publishing without preflight checks

**Data Sync (Local Changes Only):**

- `gro gitops_sync` - Fetch repo metadata, generate src/routes/repos.ts
  - Clones missing repos (with `--download`)
  - Switches branches and pulls latest changes
  - Installs dependencies if package.json changed
  - Verify repos ready without fetching (with `--check`)
  - Runs in parallel (concurrency: 5 by default)

**Command Execution (User-Defined Side Effects):**

- `gro gitops_run "<command>"` - Run shell command across all repos
  - Parallel execution (concurrency: 5 by default)
  - Continue-on-error behavior
  - Structured output (text or JSON)
  - Use for testing, auditing, batch operations

**Publishing (Git & NPM Side Effects):**

- `gro gitops_publish` - Publish packages, update dependencies, git commits

### Command Workflow

- `gitops_validate` runs: `gitops_analyze` + `gitops_plan` +
  `gitops_publish --dry_run`
- `gitops_publish` runs: `gitops_plan` (with confirmation) + actual publish

## Dependencies

- `@ryanatkn/gro` - build tool and task runner
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
  "@ryanatkn/gro": ">=0.174.0",   // controlled package - use >=
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

## Output Directory

All gitops-generated files are stored in `.gro/fuz_gitops/` (gitignored).

## Additional Documentation

- [Publishing Guide](docs/publishing.md) - Workflows, changeset semantics,
  examples
- [Troubleshooting](docs/troubleshooting.md) - Common errors and debugging tips
