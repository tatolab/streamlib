---
whoami: amos
name: "macOS compile + test verification of #322"
status: pending
description: Verify the capability-typed lifecycle ctx rewrite on macOS. Apple processor files were edited on Linux without Metal-path compile coverage; this verifies the entire macOS branch compiles, tests pass, and E2E camera→display works on real AVFoundation.
github_issue: 353
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#353

See the GitHub issue for full context.

## Priority

blocker

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
