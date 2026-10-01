---
'@fuzdev/fuz_repos': minor
---

rename the package to `@fuzdev/fuz_repos` from `@fuzdev/fuz_gitops`, and move
the site to repos.fuz.dev from gitops.fuz.dev. Consumers change the dependency
name and every `@fuzdev/fuz_gitops/*` import; the `gitops_*` task names, the
`Gitops*` identifiers and the `gitops.config.ts` filename are unchanged.
