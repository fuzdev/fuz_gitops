# TODO: gitops_run future enhancements

This doc tracks potential future enhancements to `gitops_run` beyond the initial implementation.

## Initial Implementation (v1) ✅

Core functionality for immediate use:
- Single command string execution across repos
- Parallel execution with concurrency limit (default: 5)
- Continue-on-error behavior with summary
- Output capture and structured reporting
- Uses `map_concurrent_settled` from fuz_util

**Usage:**
```bash
gro gitops_run "npm test"
gro gitops_run "npm test" --concurrency 3
gro gitops_run "npm audit" --format json
```

## Shell Features & Command Parsing

**Question: How much shell functionality should we support?**

Options:
1. **Simple string execution** (current): Just pass string to shell as-is
   - Pros: Simple, works for basic cases, supports pipes/redirects naturally
   - Cons: Shell injection risk if we ever template commands

2. **Command array** (like Node's spawn): `["npm", "test"]`
   - Pros: No shell injection possible, explicit args
   - Cons: No shell features (pipes, redirects, etc.)

3. **Template interpolation**: `"npm test {{repo_name}}"`
   - Pros: Flexible, can pass repo-specific data
   - Cons: Adds complexity, shell injection risk

**Recommendation**: Start with #1 (simple string), add #3 later if needed.

## Multiple Commands (Chaining)

Run multiple commands in sequence per repo:

```bash
# Option A: Multiple positional args
gro gitops_run "npm test" "npm audit" "gro check"

# Option B: Shell-style chaining (already works?)
gro gitops_run "npm test && npm audit && gro check"

# Option C: Config file with command sequences
# gitops.config.ts
{
  repos: [...],
  commands: {
    'full-check': ['npm test', 'npm audit', 'gro check'],
    'update': ['npm update', 'npm install'],
  }
}
```

## Conditional Execution

Run commands only on repos matching criteria:

```bash
# Only repos with changesets
gro gitops_run "npm test" --only-with-changesets

# Only repos matching pattern
gro gitops_run "npm test" --only "*_ui"

# Only repos with specific files
gro gitops_run "npm test" --only-with-file "test/**"

# Exclude repos
gro gitops_run "npm test" --exclude "fuz_template"
```

## Lifecycle Hooks in Config

Add custom commands at specific lifecycle points:

```ts
// gitops.config.ts
export default {
  repos: [...],
  hooks: {
    before_sync: "npm ci",           // Ensure clean deps before sync
    after_clone: "npm install",      // Auto-setup after download
    before_publish: "npm audit",     // Security check
    after_publish: "./notify.sh",    // Custom notifications
    on_failure: "./alert.sh",        // Alert on failures
  }
}
```

## Output Formats & Aggregation

Better output handling:

```bash
# JSON output for scripting
gro gitops_run "npm test" --format json

# Table view with status
gro gitops_run "npm test" --format table

# Only show failures
gro gitops_run "npm test" --only-failures

# Save output per repo
gro gitops_run "npm test" --save-output .gro/test_results/
```

**Output format examples:**

```json
// --format json
{
  "command": "npm test",
  "concurrency": 5,
  "repos": [
    {
      "name": "fuz_ui",
      "status": "success",
      "duration_ms": 1234,
      "stdout": "...",
      "stderr": ""
    },
    {
      "name": "fuz_css",
      "status": "failure",
      "duration_ms": 567,
      "exit_code": 1,
      "stdout": "...",
      "stderr": "..."
    }
  ],
  "summary": {
    "total": 10,
    "success": 9,
    "failure": 1,
    "duration_ms": 5678
  }
}
```

## Repo-Specific Command Overrides

Allow repos to customize commands:

```ts
// gitops.config.ts
export default {
  repos: [
    {
      repo_url: 'https://github.com/fuzdev/fuz_ui',
      commands: {
        test: 'npm test -- --coverage',  // Custom test command
        lint: 'npm run lint:strict',
      }
    },
    'https://github.com/fuzdev/fuz_css',  // Uses default commands
  ],
}
```

## Interactive Mode

Choose commands interactively:

```bash
gro gitops_run --interactive
# Prompts:
# > Select command: [test, lint, check, build, custom]
# > Select repos: [all, select, pattern]
# > Concurrency: [1, 3, 5, 10]
```

## Dry Run Mode

Preview what would run:

```bash
gro gitops_run "npm test" --dry-run
# Output:
# Would run "npm test" in 10 repos with concurrency 5:
#   - fuz_ui (~/dev/fuz_ui)
#   - fuz_css (~/dev/fuz_css)
#   ...
```

## Progress Indicators

Better UX for long-running operations:

```bash
gro gitops_run "npm test"
# Output:
# Running "npm test" in 10 repos (concurrency: 5)...
# [████████░░] 8/10 complete (fuz_ui: running, fuz_css: success, ...)
```

## Workspace State Management

Commands that need clean/dirty workspace checks:

```bash
# Require clean workspace
gro gitops_run "npm test" --require-clean

# Auto-stash before running
gro gitops_run "gro build" --auto-stash
```

## Remote Execution

Run commands on remote CI or via SSH:

```bash
# Via GitHub Actions
gro gitops_run "npm test" --remote github

# Via SSH
gro gitops_run "npm test" --remote ssh://user@host
```

## Caching & Memoization

Skip commands if nothing changed:

```bash
# Only test repos with changes since last run
gro gitops_run "npm test" --cache --since-commit HEAD~5

# Only test repos with file changes
gro gitops_run "npm test" --cache --changed-files
```

## Error Recovery Strategies

More sophisticated error handling:

```bash
# Retry failures
gro gitops_run "npm install" --retry 3

# Retry with exponential backoff
gro gitops_run "npm install" --retry 3 --backoff exponential

# Fail after N failures
gro gitops_run "npm test" --max-failures 3
```

## Dependency-Aware Execution

Run commands in dependency order:

```bash
# Build in topological order
gro gitops_run "gro build" --topo

# Test in parallel but respect dependencies
gro gitops_run "npm test" --topo-parallel
```

## Integration with Existing Commands

Reuse gitops_run patterns in other commands:

- Update `gitops_sync` to use `map_concurrent_settled`
- Add `--concurrency` flag to `gitops_publish`
- Add parallel preflight checks
- Parallel GitHub API fetching (with rate limit respect)

## Environment Variable Templating

Pass repo context as env vars:

```bash
# Template vars: REPO_NAME, REPO_DIR, REPO_URL
gro gitops_run "echo Testing $REPO_NAME"
```

## Logging & Observability

Structured logging for debugging:

```bash
# Log timing per repo
gro gitops_run "npm test" --timing

# Log resource usage
gro gitops_run "npm test" --resource-usage

# Export to observability format (OpenTelemetry, etc.)
gro gitops_run "npm test" --trace opentelemetry
```

## Config-Defined Commands

Pre-define common command sequences:

```ts
// gitops.config.ts
export default {
  repos: [...],
  commands: {
    ci: ['npm ci', 'npm test', 'npm run build'],
    update: ['gro upgrade @ryanatkn/gro@latest --no-pull', 'npm install'],
    check_all: ['gro check', 'npm audit', 'npm outdated'],
  }
}
```

```bash
gro gitops_run ci
gro gitops_run update
```

## Priority & Scheduling

Control execution order:

```ts
// gitops.config.ts
{
  repos: [
    {repo_url: '...', priority: 1},  // Run first
    {repo_url: '...', priority: 10}, // Run last
  ]
}
```

## Notes

- Keep the initial implementation simple and focused
- Add features based on actual usage patterns
- Avoid feature creep - not every shell tool needs to be reimplemented
- Consider which features are better solved by external tools (GNU parallel, etc.)
