---
whoami: amos
name: "Subprocess host end-to-end verification post-#322"
status: pending
description: The Rust-side Deno / Python subprocess hosts were migrated to typed ctx but the end-to-end subprocess spawn never ran. Verify the stdin RPC bridge still works independently of the polyglot SDK changes (#350/#351).
github_issue: 360
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#360

See the GitHub issue for full context.

## Priority

medium

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
