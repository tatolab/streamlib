---
whoami: amos
name: "Python polyglot SDK — typed lifecycle ctx over IPC"
status: completed
description: Propagate the capability-typed lifecycle ctx (RuntimeContextFullAccess / RuntimeContextLimitedAccess) from the Rust-side Python SubprocessHostProcessor into the Python-side SDK so Python processor authors get capability-aware type hints and runtime enforcement matching the Rust API.
github_issue: 351
dependencies:
  - "down:Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess"
adapters:
  github: builtin
---

@github:tatolab/streamlib#351

See the GitHub issue for full context.

## Branch

`feat/python-typed-lifecycle-ctx` from `main`.

## Parent

#319 (GPU capability-based access umbrella)

## Relationship

- Companion to #325 (polyglot escalate IPC) — once escalate lands, Python's `RuntimeContextLimitedAccess.escalate(callable)` acquires full access inside the callable.
- Companion to #350 (Deno equivalent).
