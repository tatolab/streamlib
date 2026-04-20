---
whoami: amos
name: "Release-mode E2E matrix post-#322"
status: pending
description: 'Rerun the full E2E matrix (vivid h264/h265, camera-display-only, Cam Link h264, fixture PSNR rig) in release build to catch release-only lifetime/inlining issues historically surfaced at #273/#277/#278.'
github_issue: 356
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#356

See the GitHub issue for full context.

## Priority

high

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
