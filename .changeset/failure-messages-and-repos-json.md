---
'@fuzdev/fuz_repos': patch
---

fix: `gitops_publish --format json` carries each failure's message as a string in `failed[]`, secrets masked, and `gitops_sync` compacts only the library data in `repos.json`, keeping a pull request's `draft: false`, an empty `pull_requests`, and the `package.json`'s false values and empty arrays
