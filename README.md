# fuz_repos

[<img src="/static/logo.svg" alt="a friendly blue spider facing you" align="right" width="192" height="192">](https://repos.fuz.dev/)

> a tool for managing many repos 🪄 [repos.fuz.dev](https://repos.fuz.dev/)

fuz_repos is an alternative to the monorepo pattern that more loosely couples repos:

- enables automations across repos without requiring them to be in the same monorepo
- allows each repo to be managed from multiple fuz_repos projects
- runs automations locally on your machine, giving you full control and visibility
  (big tradeoffs in both directions compared to GitHub actions)

With fuz_repos you can:

- dynamically compose repos
- fetch metadata about collections of repos and import it as typesafe JSON (`repos.ts`, written by `gro gitops_sync`)
- publish a generated docs website for your collections of repos
- import its components to view and interact with repo collection metadata
- publish metadata about your collections of repos to the web for other users and tools
- publish multiple interdependent packages in dependency order with automatic dependency updates

## Scope

fuz_repos runs **deterministic, config-driven operations over a declared set
of repos** — no LLM in the loop, and the repo set comes from a declared list
(a `repos.toml` registry, and a `gitops.config.ts` naming a subset of its keys). Publishing is its flagship capability, not its whole
identity. The Rust `repos` tool in this repo (`crates/fuz_repos`) reports
every declared repo's git state (`repos status`), syncs them (`repos sync`),
and is the gateway agents push through (`repos push`).

Deliberately out of scope: single-repo build work (that's [gro](https://github.com/fuzdev/gro)),
work whose resolution differs per repo and needs judgment, machine and server
state, and **secrets** — fuz_repos never stores, transports, or reads secret
material as data, including env-file contents. Its own GitHub API token
(noted below) is the one credential it uses, to authenticate itself.

See [CLAUDE.md](CLAUDE.md#scope-and-boundaries) for the capability tiers, what
`repos` never does, and the TS/Rust split.

## Usage

```bash
npm i -D @fuzdev/fuz_repos
```

- install the `repos` binary, which the tasks read repo state from — it isn't in the npm
  package: in a checkout of this repo, `cargo install --path crates/fuz_repos --locked`
- configure [`gitops.config.ts`](/gitops.config.ts) as a list of `repos.toml` registry keys —
  each repo's dir, URL, branch, visibility, and CI come from the registry:

  ```ts
  import type { GitopsConfig } from '@fuzdev/fuz_repos/gitops_config.ts';

  const config: GitopsConfig = { repos: ['fuz_util', 'gro', 'fuz_ui'] };

  export default config;
  ```

  The tasks find the registry the way `repos` does, walking up from the cwd, or take
  `--registry <path>`; a repo the registry has but the disk lacks is cloned by `repos sync <key>`.
- fuz_repos calls the GitHub API using the environment variable `SECRET_GITHUB_API_TOKEN` for authorization,
  which is a [classic GitHub token](https://github.com/settings/tokens)
  (with "public access" for public repos, no options selected)
  or a [fine-grained GitHub token (beta)](https://github.com/settings/tokens?type=beta)
  (with `"Public Repositories (read-only)"` selected)
  in either `process.env`, a project-local `.env`, or the parent directory at `../.env`
  (`gro gitops_sync` requires it, and you'll need to select options to support private repos)
- re-export the gitops tasks by creating files in `$lib/`:

  ```ts
  // gitops_sync.task.ts
  export * from '@fuzdev/fuz_repos/gitops_sync.task.ts';

  // gitops_analyze.task.ts
  export * from '@fuzdev/fuz_repos/gitops_analyze.task.ts';

  // gitops_plan.task.ts
  export * from '@fuzdev/fuz_repos/gitops_plan.task.ts';

  // gitops_publish.task.ts
  export * from '@fuzdev/fuz_repos/gitops_publish.task.ts';

  // gitops_validate.task.ts
  export * from '@fuzdev/fuz_repos/gitops_validate.task.ts';

  // gitops_run.task.ts
  export * from '@fuzdev/fuz_repos/gitops_run.task.ts';
  ```

- run `gro gitops_sync` to generate the dashboard's data from the repos

## Architecture

```
gitops.config.ts (registry keys) → repos status --json → local repos → GitHub API → repos.ts → UI components
```

- **Operations pattern**: Dependency injection for side effects (git, npm, fs, `repos`)
- **Fixture testing**: In-memory fixture repos with expected publishing outcomes
- **Changeset-driven**: Automatic version bumps and dependency updates

See [CLAUDE.md](CLAUDE.md#architecture) for detailed documentation.

## Quick Start

### Running commands across repos

```bash
gro gitops_run "npm test"                  # run tests in all repos (parallel, concurrency: 5)
gro gitops_run "npm audit" --concurrency 3 # limit parallelism
gro gitops_run "git status" --format json  # JSON output for scripting
```

**Features:**

- Parallel execution with configurable concurrency (default: 5)
- Continue-on-error behavior (shows all results)
- Structured output formats (text or JSON)
- Uses lightweight repo path resolution through `repos status`; a
  configured repo that's missing fails the run, naming it

### Generating the dashboard's data

```bash
gro gitops_sync               # fetch, check each repo, then write repos.json + repos.ts
gro gitops_sync --check       # the readiness report alone: no fetch, no token, nothing written
gro gitops_sync --allow_dirty # read repos off their branch, dirty, or mid-operation as they sit, warning instead
```

It reads each repo as it sits and never moves one (`repos sync` does), so it
refuses a repo off its registry branch, dirty, or mid-operation — the site
would show that tree's modules beside origin's CI — and warns on one behind
origin or missing its `node_modules`.

### Diagnostic commands (read-only)

```bash
gro gitops_validate           # run all validation checks (analyze + plan + dry run + CI reconcile)
gro gitops_analyze            # analyze dependency graph and detect cycles
gro gitops_plan               # generate publishing plan showing version changes and cascades
gro gitops_publish            # simulate publishing, writing nothing in git (dry run default)
gro gitops_publish --preview  # show the ordered side-effects a --wetrun would perform
```

These read each repo's working tree exactly as it sits on disk — no branch
switching, pulling, or installing — so they're safe to run with feature
branches checked out and uncommitted changes. Each prints the repos that
aren't at rest (off their registry branch, dirty, mid-rebase, or not in sync
with origin as of the last fetch); run `repos sync` first to read them at
rest.

### Publishing packages

```bash
gro gitops_publish --wetrun  # publish every package the plan publishes, in dependency order
gro gitops_publish --wetrun --no-plan  # skip plan confirmation
```

Before it shows the plan for confirmation, a real publish fetches every npm repo
(`repos status --fetch`) and refuses unless each is on its registry branch,
clean, idle, and in sync with origin or ahead of it, with no other live
session in its checkout — naming each problem and its fix. It moves nothing to
get there, and re-checks each repo the same way right before publishing it.

**Note:** If publishing fails, simply re-run the same command.
Already-published packages are automatically skipped (changesets consumed),
failed packages retried naturally (see
[docs/publishing.md](docs/publishing.md#readiness) for what the run leaves
unpushed).

### The `repos` tool

A Rust CLI over the repos a `repos.toml` registry declares. It moves refs it
didn't author — fetch, fast-forward, clone, push — and rebases a diverged
registry branch onto its fetched upstream when its local commits replay
without conflict. It never commits new content, resolves a conflict, merges
anything but a fast-forward, force-pushes, or pushes tags. It needs git
2.44 or newer, and Linux for its detection of live Claude Code sessions.

```bash
cargo install --path crates/fuz_repos --locked # install the `repos` binary
repos status          # git state of every entry, from local refs
repos status --fetch  # fetch from origin first (remote-tracking refs only)
repos sync            # fetch, then fast-forward, push, and clone what's safe
repos push            # push the branch checked out here, as a fast-forward
```

**Documentation:**

- ./CLAUDE.md - Architecture, commands, testing patterns
- ./docs/publishing.md - Publishing workflows, changeset
  semantics, examples
- ./docs/troubleshooting.md - Common errors and
  debugging tips
- ./docs/repos.md - The `repos` command reference

Getting started as a dev? Start with [Gro](https://github.com/fuzdev/gro)
and the [Fuz template](https://github.com/fuzdev/fuz_template).

TODO

- figure out better automation than manually running `gro gitops_sync`
- show the rate limit info
- think about how fuz_repos could use both GitHub Actions and
  [Forgejo Actions](https://forgejo.org/docs/v1.20/user/actions/)

## Contributing

[fuz.dev/contributing](https://www.fuz.dev/contributing)

## License [🐦](https://wikipedia.org/wiki/Free_and_open-source_software)

[MIT](LICENSE)
