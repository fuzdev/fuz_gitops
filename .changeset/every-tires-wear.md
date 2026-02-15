---
'@fuzdev/fuz_gitops': patch
---

- replace topological sort with `@fuzdev/fuz_util/sort.js`
- remove unused `detect_cycles()` method
- deduplicate DFS cycle detection into `#find_cycles` helper
