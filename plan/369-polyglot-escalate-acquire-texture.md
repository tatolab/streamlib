---
whoami: amos
name: "Polyglot escalate: add acquire_texture op"
status: pending
description: "Follow-up to #325. Extend the polyglot escalate IPC with AcquireTexture so Python / Deno subprocesses can reach GpuContextFullAccess::acquire_texture on behalf. MVP #325 shipped acquire_pixel_buffer + release_handle only."
github_issue: 369
dependencies:
  - "down:Polyglot IPC: escalate-on-behalf for Python/Deno processors"
adapters:
  github: builtin
---

@github:tatolab/streamlib#369

See the GitHub issue for full context.

## Priority

medium

## Parent

#319

## Depends on

#325 (lands the `EscalateOp` enum + `EscalateHandleRegistry` this ticket extends).

## Branch

`feat/polyglot-escalate-acquire-texture` from `main`.
