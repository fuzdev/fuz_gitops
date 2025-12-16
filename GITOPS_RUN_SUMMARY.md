# gitops_run Implementation Summary

## What was built

A new `gro gitops_run` command that executes shell commands across all repos in parallel with configurable concurrency and comprehensive error handling.

## Key features

1. **Parallel execution** - Runs commands across repos concurrently (default: 5)
2. **Throttled concurrency** - Uses `map_concurrent_settled` from fuz_util
3. **Continue-on-error** - Shows all results, doesn't fail-fast
4. **Structured output** - Text (default) or JSON format
5. **Lightweight** - Uses `get_repo_paths()` instead of full repo loading

## Usage

```bash
# Basic usage
gro gitops_run --command "npm test"

# Control concurrency
gro gitops_run --command "npm audit" --concurrency 3

# JSON output for scripting
gro gitops_run --command "git status" --format json

# Use with test fixtures
gro gitops_run --path src/test/fixtures/configs/basic_publishing.config.ts --command "pwd"

# Chain commands
gro gitops_run --command "gro upgrade @ryanatkn/gro@latest --no-pull && git add static/.nojekyll"
```

## Implementation details

### Files created/modified

- **NEW**: `src/lib/gitops_run.task.ts` - Main task implementation
- **NEW**: `TODO_GITOPS_RUN.md` - Future enhancement ideas
- **MODIFIED**: `src/lib/local_repo.ts` - Added parallel loading with `map_concurrent_settled`
- **MODIFIED**: `CLAUDE.md` - Added gitops_run documentation
- **MODIFIED**: `README.md` - Added usage examples

### Design choices

1. **Lightweight execution** - Uses `get_repo_paths()` instead of full `get_gitops_ready()`
   - Doesn't require `library.ts` files
   - No git sync/pull by default
   - Faster startup

2. **Shell mode** - Commands run via `sh -c` to support pipes, redirects, etc.
   - Trade-off: Slightly less safe than argument arrays
   - Benefit: Full shell capabilities

3. **Concurrency** - Default 5 repos at a time
   - Based on user preference
   - Prevents overwhelming system resources
   - Respects rate limits

4. **Error handling** - Continue-on-error with detailed reporting
   - Shows all successes and failures
   - Includes exit codes and stderr
   - Exits with error if any repo fails (for CI)

### Future enhancements (see TODO_GITOPS_RUN.md)

- Config-defined commands
- Conditional execution (--only-with-changesets, etc.)
- Lifecycle hooks
- Retry logic
- Dependency-aware execution order

## Testing

Tested with:
- Fixture repos (basic_publishing) ✓
- Real repos (9 repos in config) ✓
- Simple commands ✓
- Complex chained commands ✓
- JSON output ✓
- Various concurrency levels ✓

## Performance

Example with 9 repos @ concurrency=3:
- Simple echo command: ~41ms total
- Commands run in batches of 3
- Minimal overhead per repo (~5-20ms)
