---
whoami: amos
name: '@github:tatolab/streamlib#351'
adapters:
  github: builtin
description: Python polyglot SDK — typed lifecycle ctx over IPC — Propagate the capability-typed lifecycle ctx (RuntimeContextFullAccess / RuntimeContextLimitedAccess) from the Rust-side Python SubprocessHostProcessor into the Python-side SDK so Python processor authors get capability-aware type hints and runtime enforcement matching the Rust API.
github_issue: 351
blocks:
- '@github:tatolab/streamlib#322'
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
