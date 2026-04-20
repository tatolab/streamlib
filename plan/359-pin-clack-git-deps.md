---
whoami: amos
name: "Pin git dependencies (clack) — main is currently broken for fresh clones"
status: pending
description: `clack-host` and `clack-plugin` are unpinned git deps (track branch HEAD, not commit). When the upstream moves, fresh clones of main get a different lockfile and stop compiling. Currently main fails to build `streamlib` on a fresh clone due to this drift. Pin to a specific rev + audit the workspace for other unpinned git deps + document in CLAUDE.md.
github_issue: 359
adapters:
  github: builtin
---

@github:tatolab/streamlib#359

See the GitHub issue for full context.

## Priority

high

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
