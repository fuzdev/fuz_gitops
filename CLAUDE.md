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

## Scope and boundaries

fuz_gitops runs **deterministic, config-driven operations over a declared set
of repos — and is the gateway agents use for sensitive git operations.** Every
word is load-bearing:

- **deterministic** — no LLM in the loop. Same config plus same repo states
  produce the same plan. Everything here is reproducible and reviewable.
- **config-driven** — the repo set comes from a declared list (a project's
  `gitops.config.ts` for the TS tasks; a `repos.toml` registry for the Rust
  `repos` tool), not from scanning a directory. Scanning only reports what the
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

Multi-repo work doesn't carry uniform risk. Four tiers, ordered by blast
radius (`repos status` and `repos sync` are built; `repos push` is planned):

| Tier | Writes | Commands |
| --- | --- | --- |
| **observe** | nothing | `gitops_analyze`, `gitops_plan` (publish preview), `gitops_run` with read-only commands; `repos status` |
| **converge** | working trees, refs | `gitops_sync`; `repos sync` |
| **gateway** | remote refs, under policy | `repos push` |
| **publish** | npm + git + deploys | `gitops_publish` |

`gitops_plan` belongs to the publishing vertical but sits in **observe**
because it writes nothing. `gitops_validate` composes across tiers and belongs
to none of them. Unrelated to the converge tier: the publishing docs below say
a pass "converges" — that's ordinary fixed-point language for "no new version
changes discovered."

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

### Filling the gap: fleet git state

The **observe** tier is the one everything else should be built on: a
structured, read-only model of each repo's git state — every local branch
against its upstream, dirt split into staged/unstaged/untracked/conflicted,
local-only work, stashes, operations in progress, missing clones. The TS side
never had one (the fallback is `gitops_run "git status"` and parsing porcelain
by hand).

It's **being built** as the Rust `repos` tool, not by growing `GitOperations`:
a read-only `repos status` works today, linked worktrees and the scan for
unregistered clones included, and `repos sync` fetches, fast-forwards,
pushes, and clones what's missing; the `repos push` gateway is still to
come. Its
invariants are settled: never `pull` across a set of repos (fetch, classify,
then fast-forward or report); never rebase, merge, or auto-resolve conflicts —
anything history-changing stops and reports, so host repo rules never need
modelling; derive write authority from owner accounts; classify unpushed refs
by type.

### The TS and Rust halves

Every TS entry point is a Gro task (`src/lib/gitops_*.task.ts`), so it runs
from a Gro project, consumers add one-line re-export shims, and `--config`
defaults to the CWD's config. `gro gitops_*` stays the supported invocation for
a project's own config, publishing, and dashboard data.

**Direction, decided and underway:** the Rust side lives in this repo as one
crate, `crates/fuz_repos` (a library plus the `repos` binary), and it owns git:
repo state, sync, and the agent push path — git only, no API calls. So far it
has `repos status`: every registry entry's branches and their relation to
origin, uncommitted work in each checkout (linked worktrees too), what needs
a human, which checkouts another live Claude Code session is working in, and
the clones at the workspace root the registry doesn't name,
grouped by what to do next, from local refs (`--fetch` refreshes them
first and checks that repos declared private aren't anonymously readable),
and `repos sync`, which fetches and carries out the fast-forwards, shallow
moves, pushes, and clones `status` previews. The gateway isn't built. TS
keeps everything
else: the dashboard, its data step (GitHub metadata and svelte-docinfo library
analysis), and the publish cascade. Next, the TS tasks stop cloning and
pulling and read repo state from `repos status --json`, and a project's
`gitops.config.ts` shrinks to a list of registry keys. Nothing is deprecated
until its Rust replacement ships.

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
- `crates/fuz_repos/` - the Rust `repos` tool: registry, git runner, probe,
  unregistered scan, busy detection, classification, sync (library) and the
  `repos` binary
- `crates/fuz_repos/tests/` - its integration tests over fixture workspaces
  (`tests/support`)

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
			branch: 'main'
		}
	]
};
```

Requires `SECRET_GITHUB_API_TOKEN` in `.env` for API access.

## Main operations

### `gro gitops_sync` Task

1. Loads config from `gitops.config.ts`, and refuses to run when a public host
   package's config lists private repos (`gitops_config_leaked_private_repos`) —
   the generated `repos.json` is that package's public site data
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
	// `npm` repos (with a package.json) take part in publishing; `cargo` repos
	// (a Rust Cargo.toml, no package.json) are dashboard-only — see below.
	kind: 'npm' | 'cargo';
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

### Non-npm repos (dashboard-only)

A configured repo without a `package.json` but with a Rust `Cargo.toml` (e.g.
`tsv`) loads as a `kind: 'cargo'` `LocalRepo`. It has no npm identity, so there's
no `svelte-docinfo` analysis and no dependency graph — `local_repo.ts` synthesizes
a lightweight `Library` from the `Cargo.toml` (best-effort name/version/description,
via `cargo_toml.ts`) and the configured repo URL. These repos are still synced and
rendered on the dashboard (CI status, PRs, identity) but are excluded from
publishing and dependency analysis: `generate_publishing_plan`, `analyze_repos`,
and `execute_publishing_plan` filter to `repo_is_npm` first. A repo with neither
manifest is unsupported and fails loud.

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

The Rust `repos` tool (`crates/fuz_repos`, a Cargo workspace beside the
SvelteKit app; gro never invokes cargo):

```bash
cargo install --path crates/fuz_repos --locked # install the `repos` binary
repos status                 # git state of every repos.toml entry, local refs only, plus unregistered clones
repos status gro .           # narrow to targets: a key, a dir name, or a path inside a checkout (each names its entry); a named reference previews its refresh
repos status --verbose       # plus stash counts, unscoped sessions, each dirty worktree's own uncommitted item, and a block per entry and unregistered dir
repos status --json          # the versioned report
COLUMNS=80 repos status      # text wraps at COLUMNS (else 100); color only on a terminal without NO_COLOR
repos status --fetch         # fetch owned entries (and references asked for) from origin first (writes remote-tracking refs), and check private repos
repos status --references    # preview refreshing every third-party reference, as sync --references would (no targets with it)
repos status --jobs 4 --timings # parallelism (default 16), and per-phase timings on stderr
repos status --brief [<path>] # one line on the checkout holding the path (default: the cwd), or nothing — a SessionStart hook's nudge
repos sync                   # fetch as status --fetch does, then fast-forward, move, push, and clone what's safe; report outcomes
repos sync gro --json        # narrowed to targets; --json prints the versioned outcome report
repos sync typescript prettier # a named third-party reference is refreshed: fetched over HTTPS, then ff'd or moved where clean
repos sync --references      # refresh every third-party reference too (never a pin); alone — with targets it's a usage error
repos --version              # the crate version and the commit the binary was built from
repos --registry <file> --root <dir> status # a registry kept outside the workspace

cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
UPDATE_GOLDEN=1 cargo test --test golden # regenerate the --json golden fixtures in src/test/fixtures/repos_status/ (never hand-edit)
```

`repos` finds its registry by walking up from the cwd to the first
`repos.toml` (from a linked worktree outside the workspace, it walks up from
the repo's main checkout instead), and the **workspace root is the directory
holding it as found** —
entry dirs resolve against that root. A `repos.toml` symlink at the workspace
root pointing at a registry kept elsewhere works (the root stays the link's
dir); `--registry` naming a file in some other directory makes *that* directory
the root, unless `--root <dir>` names it. `rust-toolchain.toml` pins the
toolchain (rustup fetches it on first build), and git must be 2.44 or newer
(`GIT_NO_LAZY_FETCH` keeps a local `status` on a partial clone off the
network).

How fresh the remote view is (`fetched_at`, the footer's oldest and its
`never` count) is the newest non-empty `FETCH_HEAD` across the repo's git
dirs; a repo with no `FETCH_HEAD` at all — a fresh clone writes none — is
dated by git's own `clone: from` entry, the first line of its `logs/HEAD`
(none for a clone of an empty repo, or one whose refs are in the reftable
format, which reads as never fetched).
An empty `FETCH_HEAD` means the last fetch failed, so its age is unknown
and reads as never fetched, clone or not. In the default view each entry's
`uncommitted` item totals its primary checkout's dirt, then names its one
other dirty worktree, or folds several into a count with their summed
dirt; `--verbose` lists every dirty checkout with its dirt by kind.

`repos status --brief [<path>]` is the nudge a user-scope `SessionStart`
hook runs as `repos status --brief "$CLAUDE_PROJECT_DIR"`: its stdout lands
in the new session's context, so it prints nothing unless the checkout
holding the path has something that session should know, and one plain line
otherwise — `repos: <key> — …` naming, in order, the other live sessions
working in that checkout itself (placed there by where they are or by the
lock Claude Code put on it for them — not a session elsewhere in the repo,
which sync still counts busy in the `.claude/worktrees/` checkouts, for the
subagents it may have there), an operation in progress there, and its branch
behind or diverged from its origin upstream (with how long ago the repo was
fetched), then ahead of it (unpushed). Behind and ahead are said only for an
owned entry that isn't pinned, and dirt never (the session sees its own
working tree). It probes that entry alone, from local refs — no fetch, no
unregistered scan, nothing written — and finds the registry walking up from
the path rather than the cwd. The caller is excluded from the sessions as
anywhere (`CLAUDE_PID`, which Claude Code sets for its hooks too), and with
busy detection unavailable it says nothing of sessions. It never fails its
hook: no registry, a path in no entry (the workspace root, an unregistered
clone, outside the workspace), git missing, or a failed probe all exit `0`
in silence; only a flag it can't take (`--json`, `--fetch`, `--verbose`,
`--references`) or a second path is a usage error, exit `2`.

Under `--fetch`, each failed fetch gets a kind (`ref_gone`, `unreachable`,
`repo_not_found`, `timed_out`, …, else `failed` with git's line), and each
`[repos]` entry declared private gets an anonymous `git ls-remote` of its HTTPS
URL with no credential in reach — readable means it leaked, printed first as
`visibility`. The fetch writes remote-tracking refs and nothing else, whatever
the repo's config says; an entry whose refspecs could write outside
`refs/remotes/origin/`, or another remote's into it, isn't fetched. Origin URLs
are redacted wherever shown, and registry URLs are strict (a plain DNS host, no
userinfo or port). An origin or push URL names the registry's repo only when
read as git connects for it: the host (to the first `/` after `scheme://`, or
the first `:` in scp-like `user@host:path`) is the registry's, with no port,
and the path on it is `<account>/<name>`, case folded, a `.git` or trailing
`/` dropped — an `@` past the host, an escape (git decodes those first), a
bracket anywhere (git unwraps one into the host), a user other than ASCII
letters, digits, and `._+-`, or an IP literal never matches. The details live
in the rustdoc of `remote.rs`, `probe.rs`, and `url.rs`.

Busy detection reads the live Claude Code sessions under `$CLAUDE_CONFIG_DIR`
and `~/.claude` — `sessions/<pid>.json` and the daemon roster's workers, each
verified against `/proc/<pid>/stat`'s start time — excluding the caller
(`CLAUDE_PID`, honored only when it's an ancestor of the process). A session
works at its recorded cwd (Claude Code's `originalCwd`, which entering or
exiting a worktree rewrites), a roster worker's `worktreePath`, and its
process's current cwd (`/proc/<pid>/cwd`). It marks busy the checkout each of
those sits in, the checkout whose git dir the nearest `.git` above it names (so
a worktree moved or copied by hand is busy wherever its files are), every
checkout under the repo's main checkout's `.claude/worktrees/` (Claude Code's
subagent worktrees, whose sessions keep the parent's cwd; for a bare or
`--separate-git-dir` repo, the common dir's too, and for a moved worktree, its
own), and every checkout whose lock names it — Claude Code locks each worktree
it creates with the reason `claude <agent|session> <name> (pid <pid> start
<start>)`, matched against the session's pid and start time. A busy checkout
holds every action on its branches, pushes included. Claude Code roots agent
worktrees at its tracked cwd, which the Bash tool's `cd` moves without moving
the process or the session file, so such a worktree is caught by its lock
alone; one Claude Code doesn't lock (a `WorktreeCreate` hook's, another
tool's), and work through `GIT_DIR` or `git -C`, are placed by those paths
alone. A Claude process that writes no session file (an agent-team teammate,
a session started inside another's environment) or runs under a
`CLAUDE_CONFIG_DIR` the tool doesn't read is invisible, its locks with it, and
a change to Claude Code's lock format silently drops the lock signal.

It fails closed: a live session it can't vouch for (a file that won't parse,
another machine's or pid namespace's, no `/proc`, a path it can't resolve), a
file or dir it can't read, a relative `CLAUDE_CONFIG_DIR` or `HOME`, or `HOME`
unset makes detection unavailable, which holds every action and prints on the
`failed` line. A checkout whose path can't be resolved may be busy: it holds
the branches checked out there and shows as a `needs_human` reason. So does a
git dir no worktree list names that shares the repo's refs (a hand-made
`commondir`, or `git-new-workdir`) with a session in it, and a branch git says
is checked out in a worktree the probe didn't find. Sessions in no checkout
show only under `--verbose` and in the JSON. The details live in the rustdoc
of `sessions.rs` (the reader) and `busy.rs` (the scoping).

`repos sync` is `status --fetch` followed by acting on each branch's verdict:
a branch behind is fast-forwarded — in place when no checkout has it (a
confined `git fetch .` of the exact upstream commit, so git refuses a non-ff
and a branch checked out anywhere), in its checkout with `merge --ff-only
--no-overwrite-ignore` only when that checkout is still on it and clean — and
a shallow branch with no local commits moves to the fetched tip (`update-ref`
compare-and-swap in place, `switch -C --no-overwrite-ignore` in a clean
checkout). The live sessions are read after the fetch and again right before
each action, and each action re-checks what it relies on (and checks after
the fact what git can't refuse); git refusing is `failed`, exit `1` (as is a
failed probe, or a fetch that failed or that the tool refused to run). A
branch ahead is pushed through `origin` to its upstream's branch — `git push
origin <oid>:<ref>`, the commit classified, no force or lease (git's own
fast-forward refusal stays the last check), no tags or push options, SSH
only — once the branch still reads as classified and origin's push URL
(`git remote get-url --push --all`, `pushurl` and `pushInsteadOf` applied)
is exactly the registry's repo over SSH; any other push URL holds its pushes
as a `needs_human` reason, and an upstream at `origin/HEAD` (or outside
`refs/heads/`) is left to a person. A remote moved since the fetch makes git
refuse the push (`held`, rerun); the remote's own refusal or an unreachable
host is `push_failed`, exit `1`. **An agent's pushes are held**: under
`CLAUDECODE` (Claude Code's agent shells) every push reads `held (gateway)`
in `status` and `sync` alike, and the person runs `repos sync` to push —
until the `repos push` gateway lands, when this lifts. A branch that's a
symbolic ref never acts. It never rebases, merges anything but a
fast-forward, deletes a branch, or prunes a worktree (the fetch prunes only
remote-tracking refs gone upstream), and never touches a pin. A failed fetch
holds that entry's moves and pushes (its remote-tracking refs weren't
refreshed), and a branch on HEAD in several checkouts holds its fast-forward
or move. The rustdoc of `sync.rs` has the details.

**Third-party references are like locked dependencies**: left as they are —
never fetched, no branch compared against a remote, only local work
reported — unless the run names them as targets (a path inside a checkout
names its entry) or passes `--references`, which takes no targets (with them
it's a usage error, exit `2`). Then each is refreshed (its `refresh` verdict `act`): fetched from origin over
HTTPS alone, with the same confined fetch, and each branch fast-forwarded, or
moved when shallow, as an owned one would be where clean; never pushed, so a
branch ahead is local-only work, and one diverged or shallow with local
commits is left to a person. A pin is never fetched; named, it's refused
(`refresh` held `pinned`), and `--references` passes it over. A reference
whose `origin` isn't the registry's repo (a fork, or no URL) is never
fetched: its refresh is held (`refresh held (origin drift)`, held by
`entry`) and its origin-drift line says the fix. A refresh also needs an
HTTPS origin naming the registry's repo, as git resolves it (`insteadOf`
applied): the same repo over SSH, `http://`, or `git://`, or a rewrite of
it, holds the refresh (`refresh held (origin not HTTPS)`, held by
`origin_not_https`), never fetched, with a needs-human line saying the
`set-url` fix or naming the rewrite. `status` takes
the same targets and `--references` to preview a refresh from local refs, and
fetches it under `--fetch`. A partial clone (a `sparse` reference, cloned
`--filter=blob:none`) lacks the blobs a new tip's checkout needs: a
fast-forward or move in its checkout fetches them on demand from origin
alone, over the one transport origin's URL names (SSH or HTTPS, whoever owns
the repo), and only when no other remote is a promisor; every other call
keeps lazy fetching off.

**Each missing entry is cloned** — agents' runs included, and whether or
not busy detection can vouch for every session, since a clone only creates
a dir. Owned entries clone over SSH, third-party ones over HTTPS, each
allowed that transport alone; on the entry's `branch` (`--branch`, else the
remote's default), `--depth 1` when `shallow`, cone-mode `sparse` with
`--filter=blob:none`, with `--no-tags` and no submodules or hooks — the
`tagOpt` that records is then unset, so the user's own `git fetch` there
follows tags as in any clone. The clone
is made in a temp dir beside the entry's (`.<dir>.repos-clone-<pid>-<nonce>`,
the nonce random per process, so runs in separate pid namespaces never share
one) and moved into place only when whole, claiming the path so nothing there
is ever cloned over; a failure or timeout deletes the temp dir, and the
unregistered scan names any it finds as an unfinished clone — one a killed run
left, or one still running, to remove once no `repos sync` is. Anything at the path
— a file, an empty dir, a dangling symlink — reads as not a repo, never
missing; a live session at or under the path, or another entry's gone
worktree recorded there, holds the clone, and an entry whose `url` another
entry shares is never cloned (its dir may have been a worktree of that
repo) — a `needs_human` reason, as is a missing entry whose repo an
unregistered dir at the root already clones (its origin names it, or a rename
of it differing only in case and `-` against `_`: likely the entry's checkout
under another name). The unregistered scan runs for that whenever the run
includes a missing entry, named or not, before anything is cloned, in `sync`
as in `status` (a run with targets doesn't report what it found); a clone
whose origin names the repo by an unrelated old name isn't caught. A `sparse` path is checked at parse: plain
relative directory names, no globs. The clone is read back in place
(on its branch, tracking origin's, clean) and reported `cloned`, or
`clone_failed` (classified as a fetch failure is) or `failed`, exit `1`. The
rustdoc of `clone.rs` has the recipe.

`cargo test --workspace` runs integration tests over hermetic fixture
workspaces (`crates/fuz_repos/tests/support`): real repos in a tempdir, each
cloned from a local bare remote, with git's environment cleared (no global or
system config, fixed identities and dates) and no network — the `ssh` on
`PATH` is the fixture's own, serving pushes to the registry's SSH URLs from
the local bare remotes and refusing anything else, SSH failures come from
fakes too, `GIT_EXEC_PATH` is the fixture's — git's own programs, but a
`git-remote-https` that refuses every URL unless a test serves the bare
remotes through it (`serve_https`) — and the visibility check reads
`file://` repos and a loopback HTTP server. Busy detection reads fixture config dirs whose session
files name the tests' own child processes. They need git 2.44 or newer on
`PATH`, and Linux (`/proc`, `/etc/machine-id`). CI runs these
fmt, clippy, and test commands (with `--locked`, and `--no-fail-fast` on
tests) in the `rust` job of `.github/workflows/check.yml`, beside the gro
check.

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
- `@fuzdev/fuz_css` - semantic-first CSS framework and design system
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

`src/test/fixtures/repos_status/*.json` are the Rust `repos status --json`
and `repos sync --json` golden documents (report, narrowed report,
busy-detection states, sync report, error documents), written by
`crates/fuz_repos/tests/golden.rs` as the contract TS consumers parse
against — regenerate them with `UPDATE_GOLDEN=1 cargo test --test golden`,
never by hand.

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
