# Troubleshooting

Common errors, solutions, and debugging tips for fuz_gitops.

## Table of Contents

- [Common Errors](#common-errors)
- [Unexpected Behavior](#unexpected-behavior)
- [Debugging Tips](#debugging-tips)

## Common Errors

### "the `repos` binary was not found on PATH" or "prints status format …"

Every task reads repo state from `repos status --json`, and the `repos` binary
isn't in the npm package. Install it from a checkout of this repo at the
version of `@fuzdev/fuz_gitops` you use — its format version must match the
package's:

```bash
cargo install --path crates/fuz_repos --locked
```

### "the gitops config lists `…`, which the registry (repos.toml) doesn't name"

The config lists `repos.toml` keys, not URLs, dir names, or paths; the message
suggests the keys it resembles.

### "a configured repo can't be loaded" or "configured repos can't be loaded"

Each problem names a key in `gitops.config.ts`:

- **the dir of another entry, not a registry key** — list the key it names
- **a third-party reference** — the config lists owned repos only
- **missing** — `repos sync <key>` clones it
- **isn't a git repo** or **probing failed** — `repos status <key>` shows what
  it found
- **private, and a public package** — `gitops_sync` writes each repo's
  metadata into the host's public `repos.json`, so a public host can't list
  repos the registry declares private

When no `repos.toml` is found walking up from the cwd, pass
`--registry <path>`.

### "not publishing, and nothing was changed: … each must be ready"

`gro gitops_publish --wetrun` fetches every npm repo in the config and refuses
unless each is ready. It moves nothing to get there, so the refusal changes
nothing; each line names a repo, what's wrong, and the fix:

- **on `feature`, not `main`** (or **detached at …**) — the primary checkout
  isn't on the branch the registry entry follows: switch back once the work
  there is committed or stashed
- **uncommitted changes (… staged, … unstaged, … untracked, … conflicted)** —
  commit, stash, or discard them; untracked files count. After a failed
  publish, see [Uncommitted changes after a failed `changeset publish`](#uncommitted-changes-after-a-failed-changeset-publish)
  before committing
- **a rebase is in progress** (or merge, cherry-pick, …) — finish or abort it
- **`main` is N commits behind origin** — `repos sync <key>` fast-forwards it
- **`main` has diverged from origin** — rebase or merge by hand, then push
- **`main` tracks no upstream on origin**, **tracks an upstream gone from
  origin**, or **isn't compared with origin** — set or repoint its upstream;
  `repos status <key>` says what it found
- **fetching origin failed (…)** — fix the remote (network, auth, a gone ref)
  and rerun; `repos status --fetch <key>` retries the fetch
- **another live Claude Code session works in its checkout (pid …)** — the
  publish commits there, so wait for that session to finish
- **busy detection is unavailable (…)** — a session file couldn't be read or
  vouched for, so no checkout can be ruled out; `repos status` says why
- **origin isn't the registry's repo**, or another `needs_human` reason —
  `repos status <key>` names the fix

A branch ahead of origin isn't refused: the gate logs it, saying whether its
release push carries the commits or they stay unpushed until `repos sync` or
`repos push`.

It gates every npm repo, not just the ones being published: the plan reads
each one's changesets and versions, so a repo off its branch or behind origin
could make it wrong. The diagnostics (`gitops_plan`, `gitops_analyze`,
`gitops_validate`, the dry run) print the same problems as warnings, from
local refs, before you try.

### "npm authentication failed"

Log in to npm:

```bash
npm login
npm whoami  # verify login
```

### "Preflight checks failed: [package] failed to build"

Fix the build errors before publishing:

```bash
cd path/to/package
gro build  # See the full build error
# Fix the errors
gro build  # Verify it works
```

Build validation runs during preflight checks to prevent broken state. All
packages must build successfully before any publishing begins.

### "Plan differs from actual publish"

The dry run (`gro gitops_publish`) reports the same cascade as `gro gitops_plan`
— including bump escalations and auto-generated changesets — so the preview and
the plan always agree. If they ever disagree, regenerate both and compare.

A real publish (`--wetrun`) executes that plan and **fails loud** if a published
version diverges from the plan's prediction — it aborts with a `drift` failure
rather than silently continuing. Drift means the inputs changed underneath the
plan, e.g.:

- Another publish happened between planning and publishing
- NPM has not propagated a just-published version yet
- The working tree changed (new or edited changesets) after the plan was generated

Solution: re-run `gro gitops_publish --wetrun`. It re-plans from the current
working tree (already-published packages drop out) and continues.

### "… isn't ready to publish, re-checked right before `gro publish`"

Right before each `gro publish`, the executor re-checks that repo as the gate
did (a fresh `repos status --fetch`) and aborts before touching npm unless it's
still ready. Something changed after the gate — another session pushed to
origin, a tracked file was edited, a session started working there. The
failure's code is `not_ready`; packages published before it stay published.
Fix what it names, then re-run `gro gitops_publish --wetrun` to resume.

### A release commit and tag left local (the push was rejected)

`gro publish` doesn't check its own `git push`. If origin moved in the seconds
between the executor's re-check and that push, git rejects it, but `gro
publish` still succeeds: the version is on npm and the cascade carries on.
Git's rejection in the output is the only sign. The release commit and its
`vX.Y.Z` tag stay local, on a branch now diverged from origin, and the next
publish's gate refuses it. Merge origin's branch in (a rebase would leave the
tag on a commit the branch no longer holds), push the branch with `repos
push`, and push the tag by hand, since `repos` never pushes tags.

### Uncommitted changes after a failed `changeset publish`

A `gro publish` that fails partway through `changeset version` or `changeset
publish` can leave the tree dirty with the consumed changesets deleted. gro's
own advice there is `git reset --hard`: committing would drop the changesets,
so the retry would have nothing to publish. The readiness gate reports the dirt
without knowing which case it is — check `git status` before choosing.

### "Circular dependency detected in production dependencies"

Production/peer circular dependencies block publishing. You must:

1. Identify the cycle in `gro gitops_analyze` output
2. Move one dependency to devDependencies
3. Or restructure to remove the cycle

Note: Dev dependency cycles are normal and allowed.

### "Failed to publish: package not found on NPM after 10 minutes"

NPM propagation can be slow. Either:

- Increase timeout with `--max-wait` (default is 10 minutes / 600000ms)
- Check NPM registry status
- Verify package was actually published
- If verified published, re-run `gro gitops_publish --wetrun` to continue
  (already-published packages will be skipped)

## Unexpected Behavior

### "Auto-changeset generated when I didn't expect it"

This happens when:

- A dependency was published with a new version
- Your package has that dependency in dependencies or peerDependencies

This is correct behavior - packages must republish when their dependencies
change.

### "Why was my package deployed when it didn't publish?"

Deployment occurs for packages with ANY changes (not just published packages):

- Published in this run
- Production/peer dependencies updated
- Dev dependencies updated (requires rebuild/deploy)

This is correct behavior - dev dep changes require redeployment even without
version bumps.

### "analyze/plan show stale data or the wrong branch"

The diagnostics (`gitops_analyze`, `gitops_plan`, `gitops_validate`,
`gitops_publish` dry run) read each repo's working tree **as-is** — whatever
branch is checked out, including uncommitted changes. They do not switch
branches or pull, and move no ref (gro caches `.gro/library.json` and may
refresh the index). Each prints a "not at rest" block naming the repos off
their registry branch, dirty, mid-operation, or out of sync with origin as of
the last fetch. To run against each repo's registry branch with the latest
changes:

```bash
repos sync       # fetch, fast-forward what's behind, push what's ahead
gro gitops_plan  # then read the repos at rest
```

`repos sync` never switches a branch or touches uncommitted work: a repo off
its branch or dirty stays as it is until you move it.

### "Package not publishing even though I have a changeset"

Check:

1. Changeset file is in `.changeset/` directory
2. Changeset file is not `README.md`
3. Changeset references the correct package name
4. Changeset has valid frontmatter format

### "How does resumption work after failures?"

Resumption is **automatic** and **natural**:

1. When `gro publish` succeeds, it consumes changesets
2. A single `gro gitops_publish --wetrun` run handles the full dependency cascade:
   the plan resolves it up front and the publish executes it in one pass
3. If publishing fails mid-way, re-run `gro gitops_publish --wetrun`:
   - It re-plans from the current working tree
   - Already-published packages have no changesets → drop out of the new plan
   - Failed packages still have changesets → retried automatically
   - Repos left ahead of origin by the run's own commits pass the gate: their
     release pushes them, or, if they don't publish, they stay unpushed until
     `repos sync` or `repos push`
4. No state files needed, just re-run the same command!

This is safer than explicit state tracking because:

- No stale state files to confuse users
- No need to remember `--resume` flag
- The readiness gate catches incomplete operations
- Changeset consumption provides natural, foolproof resumption

## Debugging Tips

### View detailed dependency graph

```bash
gro gitops_analyze --format markdown --outfile deps.md
```

### Compare plan vs actual

```bash
# Before publishing
gro gitops_plan --format markdown --outfile plan.md

# After publishing (dry run, which is the default)
gro gitops_publish --format markdown --outfile actual.md

# Compare files
diff plan.md actual.md
```

### Check what changed since last publish

```bash
# In each repo
git log --oneline
ls .changeset/
```
