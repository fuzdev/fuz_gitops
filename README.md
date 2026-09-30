# fuz_gitops

[<img src="/static/logo.svg" alt="a friendly blue spider facing you" align="right" width="192" height="192">](https://gitops.fuz.dev/)

> a tool for managing many repos 🪄 [gitops.fuz.dev](https://gitops.fuz.dev/)

fuz_gitops is alternative to the monorepo pattern that more loosely couples repos:

- enables automations across repos without requiring them to be in the same monorepo
- allows each repo to be managed from multiple fuz_gitops projects
- runs automations locally on your machine, giving you full control and visibility
  (big tradeoffs in both directions compared to GitHub actions)

With fuz_gitops you can:

- dynamically compose repos
- fetch metadata about collections of repos and import it as typesafe JSON (using fuz_ui's
  [vite_plugin_pkg_json](https://ui.fuz.dev/docs/vite_plugin_pkg_json))
- publish a generated docs website for your collections of repos
- import its components to view and interact with repo collection metadata
- publish metadata about your collections of repos to the web for other users and tools
- publish multiple interdependent packages in dependency order with automatic dependency updates

## Scope

fuz_gitops runs **deterministic, config-driven operations over a declared set
of repos** — no LLM in the loop, and the repo set comes from a declared list
(a `repos.toml` registry, and a `gitops.config.ts` naming a subset of its keys). Publishing is its flagship capability, not its whole
identity. The Rust `repos` tool in this repo (`crates/fuz_repos`) reports
every declared repo's git state (`repos status`), syncs them (`repos sync`),
and is the gateway agents push through (`repos push`); the package is
renaming to `fuz_repos`.

Deliberately out of scope: single-repo build work (that's [gro](https://github.com/fuzdev/gro)),
work whose resolution differs per repo and needs judgment, machine and server
state, and **secrets** — fuz_gitops never stores, transports, or reads secret
material as data, including env-file contents. Its own GitHub API token
(noted below) is the one credential it uses, to authenticate itself.

See [CLAUDE.md](CLAUDE.md#scope-and-boundaries) for the capability tiers, what
`repos` never does, the known gaps, and the TS/Rust split.

## Usage

```bash
npm i -D @fuzdev/fuz_gitops
```

- install the `repos` binary, which the tasks read repo state from — it isn't in the npm
  package: in a checkout of this repo, `cargo install --path crates/fuz_repos --locked`
- configure [`gitops.config.ts`](/gitops.config.ts) as a list of `repos.toml` registry keys —
  each repo's dir, URL, branch, visibility, and CI come from the registry:

  ```ts
  import type { GitopsConfig } from '@fuzdev/fuz_gitops/gitops_config.ts';

  const config: GitopsConfig = { repos: ['fuz_util', 'gro', 'fuz_ui'] };

  export default config;
  ```

  The tasks find the registry the way `repos` does, walking up from the cwd, or take
  `--registry <path>`; a repo the registry has but the disk lacks is cloned by `repos sync <key>`.
- fuz_gitops calls the GitHub API using the environment variable `SECRET_GITHUB_API_TOKEN` for authorization,
  which is a [classic GitHub token](https://github.com/settings/tokens)
  (with "public access" for public repos, no options selected)
  or a [fine-grainted GitHub token (beta)](https://github.com/settings/tokens?type=beta)
  (with `"Public Repositories (read-only)"` selected)
  in either `process.env`, a project-local `.env`, or the parent directory at `../.env`
  (currently optional to read public repos, but it's recommended regardless,
  and you'll need to select options to support private repos)
- re-export the gitops tasks by creating files in `$lib/`:

  ```ts
  // gitops_sync.task.ts
  export * from '@fuzdev/fuz_gitops/gitops_sync.task.ts';

  // gitops_analyze.task.ts
  export * from '@fuzdev/fuz_gitops/gitops_analyze.task.ts';

  // gitops_plan.task.ts
  export * from '@fuzdev/fuz_gitops/gitops_plan.task.ts';

  // gitops_publish.task.ts
  export * from '@fuzdev/fuz_gitops/gitops_publish.task.ts';

  // gitops_validate.task.ts
  export * from '@fuzdev/fuz_gitops/gitops_validate.task.ts';
  ```

- run `gro gitops_sync` to sync repos and update the local data

## Architecture

```
gitops.config.ts (registry keys) → repos status --json → local repos → GitHub API → repos.ts → UI components
```

- **Operations pattern**: Dependency injection for all side effects (git, npm, fs, `repos`)
- **Fixture testing**: Generated git repos for isolated tests
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
- Uses lightweight repo path resolution through `repos status` (no full sync needed); a
  configured repo that's missing fails the run, naming it

### Syncing repo metadata

```bash
gro gitops_sync               # sync repos and generate UI data
```

### Diagnostic commands (read-only)

```bash
gro gitops_validate           # run all validation checks (analyze + plan + dry run)
gro gitops_analyze            # analyze dependency graph and detect cycles
gro gitops_plan               # generate publishing plan showing version changes and cascades
gro gitops_publish            # simulate publishing without side effects (dry run default)
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
gro gitops_publish --wetrun  # actually publish all repos with changesets
gro gitops_publish --wetrun --no-plan  # skip plan confirmation
```

Before it shows the plan for confirmation, a real publish fetches every npm repo
(`repos status --fetch`) and refuses unless each is on its registry branch,
clean, idle, and in sync with origin or ahead of it, with no other live
session in its checkout — naming each problem and its fix. It moves nothing to
get there, and re-checks each repo the same way right before publishing it.

**Note:** If publishing fails, simply re-run the same command.
Already-published packages are automatically skipped (changesets consumed),
failed packages retried naturally. Repos the run left ahead of origin (its
dependency-rewrite commits) push with their own release, or stay unpushed until
`repos sync` or `repos push` if they don't publish.

### The `repos` tool

A Rust CLI over the repos a `repos.toml` registry declares. It moves refs it
didn't author — fetch, fast-forward, clone, push — and never commits, rebases,
merges anything but a fast-forward, force-pushes, or pushes tags. It needs git
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
- think about how fuz_gitops could use both GitHub Actions and
  [Forgejo Actions](https://forgejo.org/docs/v1.20/user/actions/)

## Contributing

[fuz.dev/contributing](https://www.fuz.dev/contributing)

## License [🐦](https://wikipedia.org/wiki/Free_and_open-source_software)

[MIT](LICENSE)
