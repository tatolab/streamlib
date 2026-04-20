---
whoami: amos
name: "Verify #322 under --features moq"
status: pending
description: 'Compile + test + smoke the moq_publish_track / moq_subscribe_track processors with `--features moq` enabled. The feature was disabled during #322 review so the migration of these processors is unverified.'
github_issue: 357
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#357

See the GitHub issue for full context.

## Priority

medium

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
