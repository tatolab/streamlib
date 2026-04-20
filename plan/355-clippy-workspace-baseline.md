---
whoami: amos
name: "cargo clippy workspace baseline after #322 + edition 2024"
status: pending
description: 'Run `cargo clippy --workspace --all-targets -- -D warnings`, categorize findings (new vs. pre-existing vs. edition-2024 lints), fix or allow each with rationale, gate in CI.'
github_issue: 355
adapters:
  github: builtin
---

@github:tatolab/streamlib#355

See the GitHub issue for full context.

## Priority

medium

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
