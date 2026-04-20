---
whoami: amos
name: "Deno polyglot SDK — typed lifecycle ctx over IPC"
status: in_review
description: Propagate the capability-typed lifecycle ctx (RuntimeContextFullAccess / RuntimeContextLimitedAccess) from the Rust-side DenoSubprocessHostProcessor into the Deno-side SDK so TypeScript processor authors get the same LSP-guided, hard-to-misuse API as Rust authors.
github_issue: 350
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#350

See the GitHub issue for full context.

## Branch

`feat/deno-typed-lifecycle-ctx` from `main`.

## Parent

#319 (GPU capability-based access umbrella)

## Relationship

- Companion to #325 (polyglot escalate IPC) — once escalate lands, the Deno SDK's `RuntimeContextLimitedAccess` gains an `escalate(fn(full) => …)` helper.
- Companion to #351 (Python equivalent).
