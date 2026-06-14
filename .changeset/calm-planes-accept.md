---
'@fuzdev/fuz_gitops': minor
---

add host-state fields to repo config (groundwork)

- `RawGitopsRepoConfig` accepts optional `visibility` (`'public' | 'private'`, defaults to `'public'`), `ci`, and `archived` (defaults to `false`)
- `ci` defaults to `true` for public repos and `false` for private ones, overridable per-repo
- `reconcile_ci` flags drift between a repo's declared `ci` and its actual workflow files
- not yet consumed by sync/publish
