---
'@fuzdev/fuz_gitops': minor
---

add host-state fields to repo config (groundwork)

- `RawGitopsRepoConfig` accepts optional `visibility` (`'public' | 'private'`, defaults to `'public'`), `ci`, and `archived` (defaults to `false`)
- `ci` defaults to `true` for public repos and `false` for private ones, overridable per-repo
- `reconcile_ci` flags drift between a repo's declared `ci` and its actual workflow files, skipping archived repos
- `gro gitops_validate` now runs `ci_reconcile` and hard-fails (throws) on any error from any step — a production dependency cycle, a plan error, or CI drift — instead of completing with a warning; warnings stay non-fatal
- not yet consumed by sync/publish
