# Troubleshooting

Common errors, solutions, and debugging tips for fuz_gitops.

## Table of Contents

- [Common Errors](#common-errors)
- [Unexpected Behavior](#unexpected-behavior)
- [Debugging Tips](#debugging-tips)

## Common Errors

### "Preflight checks failed: workspace has uncommitted changes"

Commit or stash your changes before publishing:

```bash
git status
git add .
git commit -m "prepare for publish"
```

### "Preflight checks failed: not on main branch"

Switch to main branch:

```bash
git checkout main
git pull
```

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

This can happen if:

- Another publish happened between plan generation and actual publish
- NPM registry has not propagated yet

Solution: Run plan again, compare outputs.

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

### "Package not publishing even though I have a changeset"

Check:

1. Changeset file is in `.changeset/` directory
2. Changeset file is not `README.md`
3. Changeset references the correct package name
4. Changeset has valid frontmatter format

### "How does resumption work after failures?"

Resumption is **automatic** and **natural**:

1. When `gro publish` succeeds, it consumes changesets
2. Single `gro gitops_publish --wetrun` run handles full dependency cascades via
   iteration (max 10 passes)
3. If publishing fails mid-way, re-run `gro gitops_publish --wetrun`:
   - Already-published packages have no changesets → skipped automatically
   - Failed packages still have changesets → retried automatically
4. No state files needed, just re-run the same command!

This is safer than explicit state tracking because:

- No stale state files to confuse users
- No need to remember `--resume` flag
- Git workspace checks catch incomplete operations
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
