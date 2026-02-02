# Publishing Guide

This guide covers multi-repo publishing workflows, changeset semantics, and the
algorithms that power fuz_gitops publishing.

## Table of Contents

- [Quick Start](#quick-start)
- [Changeset Semantics](#changeset-semantics)
- [Plan vs Dry Run](#plan-vs-dry-run)
- [Publishing Algorithms](#publishing-algorithms)
- [Private Packages](#private-packages)
- [Workflows](#workflows)
- [Examples](#examples)

## Quick Start

```bash
# 1. Validate configuration (no side effects)
gro gitops_validate

# 2. Review what will be published
gro gitops_plan

# 3. Publish (after dry run looks good)
gro gitops_publish --wetrun
```

## Changeset Semantics

Packages can publish in four distinct scenarios:

### 1. Explicit Changesets (Normal Publishing)

- Package has `.changeset/*.md` files
- Dependency updates don't require higher bump
- Behavior: Published with version bump from changesets
- Reported as: "Version Changes (from changesets)"

### 2. Explicit Changesets with Bump Escalation

- Package has `.changeset/*.md` files specifying bump type
- BUT dependency updates require a HIGHER bump
- Behavior: Published with escalated bump (e.g., `patch` → `minor` for breaking
  dep)
- Reported as: "Version Changes (bump escalation required)"
- Example: You write `patch` changeset, but `gro` (breaking) forces `minor`

### 3. Auto-Generated Changesets

- Package has NO `.changeset/*.md` files
- BUT has production/peer dependency updates
- Behavior: Changeset auto-generated, package republished
- Reported as: "Version Changes (auto-generated for dependency updates)"
- Example: `gro` publishes → `fuz` depends on `gro` → auto-changeset for `fuz`

### 4. No Changes to Publish

- Package has NO `.changeset/*.md` files
- Package has NO production/peer dependency updates
- Behavior: Skipped (not published)
- Reported as: Informational status (not a warning)
- This is normal: Only packages with changes should publish

### Dependency Update Behavior

When a dependency is updated:

- **Production/peer deps**: Package must republish (triggers auto-changeset if
  needed)
- **Dev deps**: Package.json updated, NO republish (dev-only changes)

When a package appears in both production/peer and dev dependencies,
production/peer takes priority for dependency graph calculations.

## Plan vs Dry Run

### `gro gitops_plan`

- **Read-only prediction** - Generates a publishing plan showing what would be
  published
- Uses fixed-point iteration to resolve transitive cascades (max 10 iterations)
- Shows all 4 publishing scenarios: explicit changesets, bump escalation,
  auto-generated changesets, and no changes
- No side effects - does not modify any files or state

### `gro gitops_publish` (dry run, default)

- **Simulated execution** - Runs the same code path as real publishing
- Skips preflight checks (workspace, branch, npm auth)
- Only simulates packages with explicit changesets (can't auto-generate
  changesets without real publishes)
- Use plan for comprehensive "what would happen" analysis; use dry run to test
  execution flow

## Publishing Algorithms

### Fixed-Point Iteration (Cascade Resolution)

The publishing plan generation uses fixed-point iteration to resolve transitive
breaking change cascades:

1. **Initial pass**: Identify all packages with explicit changesets
2. **Iteration loop** (max 10 iterations):
   - Calculate dependency updates based on predicted versions
   - For each package:
     - Check if dependencies require a bump (prod/peer deps only)
     - **Bump escalation**: If existing changesets specify lower bump than
       required, escalate
     - **Auto-changesets**: If no changesets but deps updated, generate
       auto-changeset
     - Track breaking changes to propagate to dependents
   - Loop until no new version changes discovered (fixed point reached)
3. **Final pass**: Calculate all dependency updates and cascades

The 10-iteration limit prevents infinite loops while handling complex dependency
graphs. In practice, most repos converge in 2-3 iterations.

### Cycle Detection Strategy

The system uses topological sort with dev dependency exclusion:

- **Production/peer cycles**: Block publishing (error, must be resolved)
  - These create impossible ordering: Package A depends on Package B which
    depends on Package A
  - Solution: Move one dependency to devDependencies or restructure
- **Dev cycles**: Allowed and normal (warning only)
  - Dev dependencies don't affect runtime, so cycles are safe
  - Topological sort excludes dev deps (`exclude_dev=true`) to break these
    cycles
- **Publishing order**: Computed via topological sort on prod/peer deps only
  - Ensures dependencies publish before dependents
  - Deterministic and reproducible (alphabetically sorted within dependency
    tiers)
  - Dev dependencies updated in separate phase after all publishing completes
- **Dependency priority**: When a package appears in multiple dependency types,
  production/peer takes priority over dev

This strategy enables practical multi-repo patterns (e.g., shared test
utilities) while preventing runtime dependency issues.

## Private Packages

Packages with `"private": true` in package.json are excluded from publishing:

- Marked as `publishable: false` in dependency graph
- Not included in publishing order
- Dependents can still publish normally
- Use for internal tools, test utilities, dev-only packages

## Workflows

### Safe Validation Workflow

Before publishing, always validate your configuration:

```bash
# 1. Run comprehensive validation (no side effects)
gro gitops_validate

# 2. Review analyze output
gro gitops_analyze

# 3. Review plan to see what will be published
gro gitops_plan

# 4. Test with dry run (default)
gro gitops_publish

# 5. If everything looks good, actually publish
gro gitops_publish --wetrun
```

### Output Formats

Save analysis or plans to files for review:

```bash
gro gitops_analyze --format json --outfile analysis.json
gro gitops_plan --format markdown --outfile plan.md
```

## Examples

### Publishing a single package with changesets

```bash
# Create a changeset for your package
cd packages/my-package
npx changeset
# Follow prompts to describe changes

# Generate plan to see what will be published
gro gitops_plan
# Output shows: my-package: 1.0.0 → 1.1.0 (minor)

# Publish
gro gitops_publish --wetrun
```

### Publishing multiple packages with cascading dependencies

```bash
# You have changesets in @my/core
# Dependents: @my/ui depends on @my/core

# Plan shows cascade
gro gitops_plan
# Output:
#   @my/core: 1.0.0 → 2.0.0 (major, BREAKING)
#   @my/ui: 1.5.0 → 2.0.0 (auto-changeset, BREAKING cascade)

# Publish in dependency order
gro gitops_publish --wetrun
```

### Recovering from failures (natural resumption)

```bash
# Publishing failed midway through
gro gitops_publish --wetrun
# Error: Failed to publish @my/package-5

# Fix the issue, then re-run the same command
gro gitops_publish --wetrun
# Already-published packages have no changesets → skipped automatically
# Failed packages still have changesets → retried automatically
```

### Bump escalation

```bash
# You created a patch changeset for @my/app
# But @my/core (dependency) has a breaking change

# Plan shows escalation
gro gitops_plan
# Output:
#   @my/core: 1.0.0 → 2.0.0 (major, BREAKING)
#   @my/app: 2.0.0 → 3.0.0 (patch → major, escalated)

# Publish handles escalation automatically
gro gitops_publish --wetrun
```
