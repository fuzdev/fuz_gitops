# fuz_gitops

> Multi-repo management - alternative to monorepo pattern

fuz_gitops (`@fuzdev/fuz_gitops`) loosely couples repos with cascade publishing
and cross-repo automation.

For coding conventions, see Skill(fuz-stack).

## Table of Contents

- [Scope and boundaries](#scope-and-boundaries)
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

The Rust `repos` tool's command reference is [docs/repos.md](docs/repos.md).

## Scope and boundaries

fuz_gitops runs **deterministic, config-driven operations over a declared set
of repos — and is the gateway agents use for sensitive git operations.** Every
word is load-bearing:

- **deterministic** — no LLM in the loop. Same config plus same repo states
  produce the same plan. Everything here is reproducible and reviewable.
- **config-driven** — the repo set comes from a declared list (a `repos.toml`
  registry, and a project's `gitops.config.ts` naming a subset of its keys for
  the TS tasks), not from scanning a directory. Scanning only reports what the
  list misses.
- **operations** — it acts, or faithfully previews acting. Observation that
  never leads to an action belongs in whatever tool owns your policy checks.
- **the gateway** — agents push through the tool instead of raw git, so write
  policy lives in one place. Write authority is derived from the registry's
  owner accounts, never declared per repo.

The package is renaming to **fuz_repos**, with a `repos` binary: "GitOps" names
the inverse relationship (git as the desired state, infrastructure as the
target), where this tool treats the repos themselves as the target.

Publishing is the flagship vertical of the TS side, not the identity.

### Capability tiers

Multi-repo work doesn't carry uniform risk, so the tiers are keyed by what a
command may write, ordered by blast radius. A command maps onto every row it
writes: `repos sync` fetches, converges, and pushes; `repos push` fetches and
pushes.

| Writes | Tier | Commands |
| --- | --- | --- |
| nothing in git | **observe** | `repos status` (local refs), `repos status --brief`; `gitops_analyze`, `gitops_plan`, `gitops_publish` (dry run), `gitops_validate`, `gitops_sync --check`, `gitops_run` with read-only commands |
| remote-tracking refs only, plus the objects, `FETCH_HEAD`, and shallow boundary a fetch writes | **observe** (refreshed) | `repos status --fetch`; the fetch that starts `repos sync` and `repos push`, the readiness gate before `gitops_publish --wetrun`'s prompt and its re-check before each `gro publish`, and `gitops_sync`'s fetch before it writes its host's site data |
| local branches, working trees, new clones | **converge** | `repos sync` (fast-forwards, shallow moves, clones of missing entries, references refreshed when named or under `--references`) |
| remote branches, under policy | **gateway** | `repos push`, and the push step of `repos sync`: fast-forwards only, under a lease, to the registry's SSH URL, owned entries only, no tags or force; a new remote branch is `repos push --new-branch`, the user's |
| releases: npm, git commits and tags, deploys | **publish** | `gitops_publish --wetrun`, and gro's own `publish` and `deploy` it runs — the user's |

"Nothing in git" is exact for `repos status`; the TS observe tasks move no
ref, though gro's `git status` may refresh a repo's index, and they cache
each repo's library metadata (gro's `library_load_from_repo`) at
`.gro/library.json` inside that repo, only at a clean commit; `--outfile`
writes where it's told. `gitops_sync` writes its own host's site data
(`repos.json`, `repos.ts`, in the routes dir or `--outdir`) and its GitHub
fetch cache under the host's `.gro/`; in the other repos it writes only the
library cache the observe tasks keep too. A fetch stays in **observe**: it
refreshes the view of origin and moves no branch anyone works on.
`gitops_plan` and the publish dry run belong to the publishing vertical but
sit in **observe**: they write nothing in git. `gitops_validate` composes
analyze, plan, the publish dry run, and `ci_reconcile`, each in its observe
form.
Unrelated to the converge tier: the publishing docs below say a pass
"converges" — that's ordinary fixed-point language for "no new version
changes discovered."

### The authoring rule

`repos` moves refs it didn't author — it fetches, fast-forwards, moves
shallow branches, clones, and pushes commits that already exist — and reports
git state. The TS tasks and gro author content — commits, changesets, release
tags — and own package meaning: npm, changesets, the dependency graph, the
GitHub API, library analysis, the dashboard. Release pushes (`gro publish`'s
push with `--follow-tags`, `gro deploy`'s push to the deploy branch) stay
gro's, in the publish tier.

### What `repos` never does

Each is structural, in the code (the rustdoc of `sync.rs`, `push.rs`,
`clone.rs`, `git.rs`, `probe.rs` (fetch confinement), and `busy.rs` and
`sessions.rs` (busy detection) says how):

- never `pull`s, rebases, merges anything but a fast-forward, or resolves a
  conflict: anything history-changing stops and reports, so host repo rules
  never need modelling
- never creates a commit, a tag, or a changeset
- never force-pushes: a push is a fast-forward of exactly the tip the fetch
  saw, under a lease, and sends no tags or push options; a remote branch is
  created only by the user's `repos push --new-branch`, refused in an agent's
  shell
- never deletes a branch, local or remote — the fetch's `--prune` deletes
  only remote-tracking refs gone upstream
- never prunes or removes a worktree, and never switches a checkout to
  another branch: a fast-forward or move in a checkout runs only after
  reading it on that branch and clean (a switch by another hand in the
  instant between is the window left; `sync.rs` says what each action checks
  after)
- never touches a pin once it's cloned, refreshes a third-party reference
  only when the run names it or passes `--references`, and writes to a remote
  only for owned entries
- never acts on a branch checked out where it finds another live Claude Code
  session working (busy — [what it can't see](docs/repos.md#busy-detection)),
  and holds every branch action when it can't vouch for the sessions — a
  clone of a missing entry still runs, since it only creates a dir
- never runs hooks, an fsmonitor, or background maintenance; programs the
  user's own git config names (filter drivers, the gpg program, credential
  helpers, SSH) run as in any git call
- never installs, builds, calls the GitHub API, or reads env files: git is
  the only program it runs (and `kill`, to stop a timed-out call), and it
  holds no credential of its own

What each command writes, exactly — the fetch's confinement, sync's writes,
clone placement, `--new-branch`'s upstream config, partial clones' lazy
fetching — is in docs/repos.md: [status](docs/repos.md#repos-status),
[Fetching](docs/repos.md#fetching), [sync](docs/repos.md#repos-sync),
[`--new-branch`](docs/repos.md#--new-branch), and
[clones](docs/repos.md#cloning-missing-entries).

### Out of scope

- **Single-repo work** — build, check, publish, gen, format. That's `gro`. (A
  single-repo *push* by an agent is the exception: it goes through the gateway.)
  fuz_gitops orchestrates gro across many repos and delegates hard: it carries
  no install or cache-healing logic of its own because gro's install path
  already self-heals npm's stale-cache failure. If gro can do it for one repo,
  fuz_gitops's job is ordering and reporting, not reimplementation.
- **Work that needs judgment per repo** — a refactor whose resolution differs
  in each repo, a migration with per-repo edge cases. The rule that follows:
  **where a plan would have to guess, it must stop and report rather than
  guess.** A tool that resolves ambiguity on your behalf across many repos
  multiplies its mistakes.
- **Secrets and env files.** fuz_gitops never stores, transports, or reads
  secret material as data — not as a convenience, not behind a flag. Its own
  operating credential (`SECRET_GITHUB_API_TOKEN`, read from `.env` and sent
  only to the GitHub API) is the tool authenticating itself, not fleet secrets
  passing through it. A gitops config is committed and often public-adjacent;
  a tool that reads it should never be somewhere secrets could land. Reporting
  an env file's *presence* is legitimate fleet state; its contents never are.
- **Machine and server state.** Provisioning and deployment convergence is a
  different target with its own tooling.

### The TS and Rust halves

Every TS entry point is a Gro task (`src/lib/gitops_*.task.ts`), so it runs
from a Gro project, consumers add one-line re-export shims, and `--config`
defaults to the CWD's config. `gro gitops_*` stays the supported invocation for
a project's own config, publishing, and dashboard data.

The Rust side lives in this repo as one crate, `crates/fuz_repos` (a library
plus the `repos` binary), and moves refs under the authoring rule above — git
only, no API calls. `repos status` reports each entry's git state from local
refs (`--fetch` refreshes them first), `repos sync` carries out the
fast-forwards, moves, pushes, and clones `status` previews, and `repos push`,
the gateway, pushes one checkout's branch through sync's own push — each in
[docs/repos.md](docs/repos.md#commands).

The structured model of each repo's git state — every local branch against
its upstream, dirt by kind, local-only work, stashes, operations in progress,
missing clones — is the Rust tool's alone. The TS side has no git-state model
of its own: it consumes `repos status --json` rather than growing
`GitOperations` or parsing `git status` porcelain.

TS keeps everything else: the dashboard, its data step (GitHub metadata and
svelte-docinfo library analysis), and the publish cascade. A project's
`gitops.config.ts` is a list of registry keys, and every TS task resolves
them through `repos status <keys…> --json` — each repo's dir, URL, branch,
visibility, `ci`, and `archived` come from the registry, and the TS side
clones nothing. Readiness is read from the report too: a real publish
gates on each npm repo's `at_rest` facts after a `--fetch`, and the
diagnostics report the repos not at rest, and `gitops_sync` refuses to
generate the dashboard's data from a repo off its branch, dirty, or
mid-operation. No TS task switches a branch, pulls, or installs: moving
repos is `repos sync`'s, and installing dependencies is out of scope.

## Core functionality

- Fetches metadata from repo collections via GitHub API
- Reads each repo's git state from the `repos` binary, never moving a repo
- Generates typesafe JSON from package.json and exported modules metadata
- Publishes docs websites for repo collections
- Tracks CI status and pull requests

## Architecture

```
gitops.config.ts (registry keys) -> repos status --json -> local repos -> GitHub API -> repos.ts -> UI components
```

### Key files

- `gitops.config.ts` - user config listing the repos by registry key
- `src/lib/gitops_config.ts` - config schema, loading, and the public-host
  leak guard
- `src/lib/gitops_task_helpers.ts` - `resolve_gitops_repos` (config keys →
  `repos status` → checkouts) and `get_gitops_ready` (plus library loading),
  shared by every task; `gate_publish_readiness` and `log_readiness_block`
- `src/lib/repos_status_load.ts` - runs `repos status --json` through the
  injected `ReposOperations` and parses its report or error document
- `src/lib/repo_readiness.ts` - pure readiness predicates over the report's
  entries: at rest (the diagnostics' block), ready to publish (the gate),
  and ready to generate (`gitops_sync`)
- `src/lib/gitops_sync.task.ts` - generates the dashboard's data from each
  repo as it sits, refusing one not ready to generate from
- `src/lib/gitops_analyze.task.ts` - analyzes dependencies and changesets
- `src/lib/gitops_plan.task.ts` - generates publishing plan
- `src/lib/gitops_publish.task.ts` - publishes repos in dependency order
- `src/lib/gitops_validate.task.ts` - runs all validation checks
- `src/lib/local_repo.ts` - resolves config keys against the report
  (`local_repos_resolve`) and loads each repo's library as it sits
- `src/lib/github.ts` - GitHub API client for PRs, CI status
- `src/lib/fetch_repo_data.ts` - fetches remote repo metadata
- `src/routes/repos.ts` - generated data file with all repo info
- `src/lib/repos_status.ts` - zod mirror of the `repos status --json`
  document (report and error document), guarded by the goldens
- `crates/fuz_repos/` - the Rust `repos` tool: a library — registry, git
  runner, probe, unregistered scan, busy detection, classification, sync,
  push, and the entry points that load a registry and return a finished
  report (`status_report`, `sync_report`, `push_report`) — and the `repos`
  binary, which parses arguments, renders, and owns exit codes
- `crates/fuz_repos/tests/` - its integration tests over fixture workspaces
  (`tests/support`)
- `docs/repos.md` - the `repos` command reference

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
- Commits the run made that no release pushed (dependency rewrites and
  auto-changesets in repos it didn't publish, private update-only leaves, and
  every dev-dep bump, which lands after all publishes) leave those branches
  ahead of origin. The readiness gate passes a branch ahead: a repo
  that publishes pushes them with its release, and one that doesn't keeps them
  unpushed until `repos sync` or `repos push`

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
import type { GitopsConfig } from '@fuzdev/fuz_gitops/gitops_config.ts';

const config: GitopsConfig = {
	repos: ['fuz_util', 'gro', 'fuz_ui'] // repos.toml registry keys, in display order
};

export default config;
```

The config lists owned repos by their `repos.toml` key and nothing else: each
repo's dir, URL, branch, visibility, `ci`, and `archived` come from the
registry through `repos status <keys…> --json`, so the tasks need the `repos`
binary on `PATH` (`cargo install --path crates/fuz_repos --locked`, from a
fuz_gitops checkout — the npm package doesn't carry it) and a registry
`repos` can find from the cwd, or `--registry <path>`. The default export
may also be a function returning the config. Every task resolves the keys
the same way and refuses to run, naming each problem, when a key is
unknown, names a third-party reference, or its repo is missing (`repos sync
<key>` clones it), isn't a git repo, or failed to probe. Repos keep the
config's order.

Requires `SECRET_GITHUB_API_TOKEN` in `.env` for API access.

## Main operations

### `gro gitops_sync` Task

Generates the dashboard's data from each repo as it sits — it never switches
a branch, pulls, or installs:

1. Loads the config's registry keys and resolves them through `repos status`
   (local refs), refusing to run when a public host package's config lists
   repos the registry declares private (`gitops_config_leaked_private_repos`)
   — the generated `repos.json` is that package's public site data
2. Refuses, before any network, a repo off its registry branch, dirty
   (untracked files count), or mid-operation, naming each with its fix
   (`check_gen_readiness`): the site would show that tree's modules beside
   origin's CI. `--allow_dirty` reads such repos as they sit, warning instead.
   `--check` stops here: it logs the report and exits non-zero when a real
   run would refuse — no fetch, no token, nothing written
3. Reads `SECRET_GITHUB_API_TOKEN`
4. Fetches the repos from origin (`repos status <keys…> --fetch --json`) and
   checks them again, warning on a followed branch not in sync with origin
   (behind: CI is origin's tip, the modules the local tree) and a failed
   fetch (the last fetch's view stands)
5. Warns on each npm repo the library analysis can't fully read — no
   `node_modules`, or no `.svelte-kit/tsconfig.json` its tsconfig extends
   (external types then read as `any`); it installs nothing
6. Loads each repo's library, fetches GitHub data (CI, PRs), and writes
   `repos.json` + `repos.ts`, then updates the fetch cache

### Data fetching

- Pull requests via GitHub API
- CI check runs and status
- Package metadata from .well-known endpoints
- Caches responses to minimize API calls

### Multi-repo publishing

#### Publishing Workflow

- `gro gitops_publish --wetrun` - publishes repos in dependency order
  - Runs the readiness gate before showing the plan for confirmation (see
    below); a refusal, or declining the prompt, changes nothing
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
- Preflight checks validate changesets, builds, and npm authentication
  (skipped for dry runs); repo git state is the readiness gate's

**Readiness Gate (Read-Only)**

A real publish never moves a repo to get it ready — it checks, and refuses.
After generating the plan and before the prompt, `gate_publish_readiness`
fetches every npm repo in the config (not just the ones the plan publishes)
and refuses unless each is at rest, in sync with origin or ahead, not busy,
and free of `needs_human` reasons (`repo_readiness.ts`). The executor
re-checks each repo right before its `gro publish` (`repos status --fetch
--json <key>` through `ops.repos`) and aborts with a `not_ready` failure,
before npm, unless it's still ready. The windows left: the seconds between
that re-check and gro's push, and the dependency-rewrite commits, which aren't
re-checked and land on whatever branch the repo is on then. Detail: [docs/publishing.md](docs/publishing.md#readiness).

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
   - Runs `gro publish --no-build --no-pull --branch <entry branch>` for each
     package, right after its re-check found the branch in sync with origin or
     ahead of it, so gro's own `git pull` would only move what the check
     vouched for, and `--branch` makes gro
     check out the branch the registry entry follows rather than its `main`
     default
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
   - Both run in the foreground (`ProcessOperations.run_interactive`): output
     live, stdin the terminal's for npm's one-time-password prompt, and the
     child's stdout on stderr when the task's stdout carries `--emit_json` or a
     JSON/markdown report. A failure's message carries the end of the child's
     stderr, secrets redacted.

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
   build or deploy (`gro deploy` builds fresh).

This is why `gro publish --no-build` is safe immediately after a publish: its
internal install heals the cache. `--no-pull` skips only gro's `git pull` (and
the clean-workspace check that guards it — the gate checked); its install
still runs. Nothing installs before preflight either: preflight's `gro build`
installs as any `gro build` does. There is no `--skip-install` flag — there are
no executor-owned installs to skip.

**Plan vs Dry Run**

`gro gitops_plan` is the read-only report of the plan; the dry run
(`gro gitops_publish`, the default) consumes that same plan and reports the
same full cascade, skipping the readiness gate and preflight — the framing
differs, not the content. Both, like `gitops_analyze` and `gitops_validate`,
read repos as they sit and print a readiness block (warnings, not failures)
naming each npm repo not at rest. Detail:
[docs/publishing.md](docs/publishing.md#plan-vs-dry-run).

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
_without_ a changeset (it won't republish). A private package carrying its own
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
- `dependency_graph.ts` - Topological sorting, cycle detection, and wildcard analysis
- `graph_validation.ts` - Shared cycle detection and publishing order
  computation
- `version_utils.ts` - Version comparison and bump type detection
- `npm_registry.ts` - NPM availability checks with retry
- `dependency_updater.ts` - Package.json updates with changesets
- `repo_readiness.ts` - The readiness predicates the gate and the diagnostics'
  block read
- `preflight_checks.ts` - Pre-publish validation: changesets, builds, npm auth
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
allowed (reported as info, excluded from topological sort). Publishing order
computed via topological sort on prod/peer deps only.

## Data types

```ts
class Repo {
	readonly library: Library;
	check_runs: GithubCheckRunsItem | null;
	pull_requests: Array<GithubPullRequest> | null;
}

interface LocalRepo {
	// `npm` repos (with a package.json) take part in publishing; `cargo` repos
	// (a Rust Cargo.toml, no package.json) are dashboard-only — see below.
	kind: 'npm' | 'cargo';
	library: Library;
	package_json: PackageJson;
	repo_dir: string;
	entry: ReposEntryStatus; // the registry entry as `repos status --json` reported it
	dependencies?: Map<string, string>;
	dev_dependencies?: Map<string, string>;
	peer_dependencies?: Map<string, string>;
}

interface LocalRepoPath {
	repo_name: string; // the registry key
	repo_dir: string; // the workspace root joined with the entry's dir
	repo_url: string;
	entry: ReposEntryStatus;
}
```

### Non-npm repos (dashboard-only)

A configured repo without a `package.json` but with a Rust `Cargo.toml` (e.g.
`tsv`) loads as a `kind: 'cargo'` `LocalRepo`. It has no npm identity, so there's
no `svelte-docinfo` analysis and no dependency graph — `local_repo.ts` synthesizes
a lightweight `Library` from the `Cargo.toml` (best-effort name/version/description,
via `cargo_toml.ts`), falling back to the registry key and URL. These repos are still
rendered on the dashboard (CI status, PRs, identity) but are excluded from
publishing and dependency analysis: `generate_publishing_plan`, `analyze_repos`,
and `execute_publishing_plan` filter to `repo_is_npm` first. A repo with neither
manifest is unsupported and fails loud.

## UI components

Pages compose a detail component between `PageHeader.svelte` and
`PageFooter.svelte`:

- `TablePage.svelte` → `ReposTable.svelte` - dependency matrix view
- `TreePage.svelte`, `TreeItemPage.svelte` → `ReposTree.svelte` (with
  `ReposTreeNav.svelte`) - hierarchical repo browser
- `ModulesPage.svelte` → `ModulesDetail.svelte` (with `ModulesNav.svelte`) -
  module exploration
- `PullRequestsPage.svelte` → `PullRequestsDetail.svelte` - PR tracking

## Commands

```bash
npm i -D @fuzdev/fuz_gitops

# Dashboard data (reads each repo as it sits; `repos sync` moves them)
gro gitops_sync               # fetch, check each repo is ready, then write repos.json + repos.ts from GitHub and library data
gro gitops_sync --check       # the readiness report alone, from local refs: no fetch, no token, nothing written; non-zero when a run would refuse
gro gitops_sync --allow_dirty # read repos off their branch, dirty, or mid-operation as they sit, warning instead of refusing
gro gitops_sync --outdir <dir> # write the data somewhere other than the routes dir
gro gitops_sync --registry ../repos.toml # every task takes a registry repos wouldn't find from the cwd

# Run commands across repos (reads repos as-is, no branch switch/pull; a missing repo fails the run)
gro gitops_run "npm test"                          # run command in all repos (parallel, concurrency: 5)
gro gitops_run "npm audit" --concurrency 3         # limit parallelism
gro gitops_run "gro check" --format json           # JSON output (logged to stdout)
gro gitops_run "gro check" --format json --outfile out.json # clean JSON to a file

# Publishing
gro gitops_validate              # validate configuration (runs analyze, plan, dry run, and ci_reconcile)
gro gitops_analyze               # analyze dependencies and changesets
gro gitops_plan                  # generate publishing plan
gro gitops_plan --verbose        # show additional details
gro gitops_publish               # dry run (default, simulates publishing)
gro gitops_publish --wetrun      # actually publish repos in dependency order
gro gitops_publish --wetrun --no-plan # skip interactive plan confirmation
gro gitops_publish --verbose     # show additional details in plan
gro gitops_publish --preview     # print the ordered side-effects a --wetrun would perform
gro gitops_publish --emit_json   # stream structured publishing events as JSON-lines to stdout

# Output formats (analyze, plan, publish)
gro gitops_analyze --format json --outfile analysis.json
gro gitops_plan --format markdown --outfile plan.md

# Development
gro dev        # start dev server
gro build      # build static site
gro deploy     # deploy to GitHub Pages

# Fixtures
gro test src/test/fixtures/check # validate the plan and dry run against fixture expectations
```

The Rust `repos` tool lives in `crates/fuz_repos`, a Cargo workspace beside
the SvelteKit app; gro never invokes cargo. Its commands and flags — status,
`--brief`, fetching, sync, push, references, clones, exit codes, and the test
harness — are in [docs/repos.md](docs/repos.md#commands); install and gates:

```bash
cargo install --path crates/fuz_repos --locked # install the `repos` binary (git 2.44+; busy detection needs Linux)

cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
UPDATE_GOLDEN=1 cargo test --test golden # regenerate the --json golden fixtures in src/test/fixtures/repos_status/ (never hand-edit)
```

**Agents push through `repos push`**, never raw `git push` — a Claude Code
deny rule on the latter is guidance, not a boundary
([docs/repos.md](docs/repos.md#agents-push-through-repos-push)).

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
- `@fuzdev/fuz_css` - semantic-first CSS framework and design system
- `@sveltejs/kit` - web framework
- `svelte` - UI framework
- `zod` - schema validation

## General Patterns

- Uses Gro's well-known package.json patterns for metadata
- Generates static JSON for fast client-side rendering
- Caches API responses to minimize API calls
- Atomic file updates with format checking
- Repo dirs come from the registry, never the config
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
process, build, and the `repos` binary) are defined as interfaces. Tests provide
mock implementations; `create_mock_repos_ops` injects a `repos status --json`
document, so no test spawns the binary.

**Benefits:**

- **No mocking libraries** - Just plain objects implementing interfaces
- **Type-safe tests** - Mock implementations must match interface signatures
- **Easy setup** - Return exactly what you want from fake operations
- **Fast tests** - No real git/npm/fs operations, instant execution
- **Predictable** - Control all side effects explicitly
- **Readable** - Test code shows exactly what operations do

**Example:**

- Production: `publish_repos(repos, options)` — `options.ops` defaults to
  `default_gitops_operations`
- Tests: `publish_repos(repos, {...options, ops: create_mock_gitops_ops()})`

See `src/lib/operations_defaults.ts` for real implementations,
`src/test/test_helpers.ts` and `src/test/fixtures/mock_operations.ts` for the
mock factories.

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
gro test src/test/fixtures/check # validate the analysis, plan, and dry run against fixture expectations
```

Core modules tested:

- `version_utils.test.ts` - Version comparison and semver logic
- `changeset_reader.test.ts` - Changeset parsing and version prediction
- `dependency_graph.test.ts` - Topological sorting and cycle detection
- `changeset_generator.test.ts` - Auto-changeset content generation
- `preflight_checks.test.ts` - Changeset, build, and npm validation
- `repo_readiness.test.ts` - Readiness predicates, the gate's refusal,
  `gitops_sync`'s policy, the diagnostics' block
- `gitops_publish.test.ts` - The gate's order in a real publish: before the
  prompt and every side effect
- `gitops_sync.test.ts` - `gitops_sync`'s refusals and warnings, `--check`,
  and the order of its reads before anything is fetched or written
- `dependency_updater.test.ts` - Package.json updates and git commits

### Fixture Testing

The fixture system builds `LocalRepo`s in memory from fixture data, with mock
operations standing in for git, npm, and the fs:

**Fixture Data:**

- `src/test/fixtures/repo_fixtures/*.ts` - Source of truth for test repo definitions
- `src/test/fixtures/load_repo_fixtures.ts` - Converts a fixture to `LocalRepo`s
- `src/test/fixtures/configs/*.config.ts` - Each fixture's repos as a key-list
  config, load-validated against the fixture (there's no fixture registry, so
  the tasks don't run on them)

**Fixture Scenarios:**

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

- `src/test/fixtures/check.test.ts` - Checks the analysis, plan, and dry run against each fixture's `expected_outcomes`
- `src/test/fixtures/helpers.ts` - Assertion helpers

**Workflow:**

1. Define fixture data with expected outcomes in `repo_fixtures/*.ts`
2. Run `gro test src/test/fixtures/check` to validate the plan and dry run
   against expected outcomes

Each fixture runs in isolation, validating:

- Publishing order (topological sort correctness)
- Version changes (explicit, auto-generated, bump escalation scenarios)
- Breaking change cascades
- Warnings, errors, and info messages

The Rust test harness is described in [docs/repos.md](docs/repos.md#testing),
along with the `repos --json` golden documents in
`src/test/fixtures/repos_status/` (regenerated with `UPDATE_GOLDEN=1 cargo
test --test golden`, never by hand). `src/test/repos_status.golden.test.ts`
parses the status goldens with the strict schemas of `src/lib/repos_status.ts`,
and the TS consumer's own tests take them as inputs: the parser
(`repos_status_load.test.ts`, each error document mapped to its message) and
the key resolution (`local_repo.resolve.test.ts`).

## Generated Files & Caches

- **Repo data** — `gro gitops_sync` writes `repos.json` + `repos.ts` to the
  SvelteKit routes dir (`src/routes/` by default, overridable with `--outdir`).
  These are committed (the site renders from them).
- **Caches** (gitignored, under `.gro/`) — the fetch-value cache at
  `.gro/build/fetch/`, and, in each repo analyzed, the `svelte-docinfo`
  library metadata at `.gro/library.json` (written by gro's
  `library_load_from_repo`, keyed by a clean `HEAD`).

## Additional Documentation

- [Publishing Guide](docs/publishing.md) - Workflows, changeset semantics,
  examples
- [Troubleshooting](docs/troubleshooting.md) - Common errors and debugging tips
- [The `repos` tool](docs/repos.md) - Command reference: registry discovery,
  status, fetching, busy detection, sync, push, references, clones, testing
