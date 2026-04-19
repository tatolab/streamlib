---
whoami: amos
name: "Design doc: GpuContextSandbox + GpuContextFullAccess API surface"
status: pending
description: Write the design doc gating all downstream work in #319. Output is a reviewable document, not code.
github_issue: 320
adapters:
  github: builtin
---

@github:tatolab/streamlib#320

## Branch

`design/gpu-capability-sandbox` from `main`. Doc-only PR.

## Deliverable

`docs/design/gpu-capability-sandbox.md` covering:

- **API split table**: every current `GpuContext` method classified as `Sandbox`, `FullAccess`, or `Both`. Pool acquire (pre-reserved) → Sandbox. `vmaCreate*` / new texture / new session → FullAccess. Ambiguous cases called out.
- **Escalate closure signature**: sync vs async, `FnOnce` vs `FnMut`, error propagation, `FullAccess` lifetime scoped to the closure.
- **Compiler integration**: how `spawn_processor_op.rs` Phase 4 wraps setup in `escalate()`. What today's setup mutex becomes (primitive, consumed by `escalate` — not a peer).
- **RuntimeContext changes**: sandbox vs full-access accessors. `ReactiveProcessor::setup` / `process` signature impact.
- **Polyglot mapping**: IPC schema for escalate-on-behalf; subprocess-host routing.
- **Migration plan**: order of operations for tasks #321–#326. Which changes are compile-only vs behavior-changing.
- **Alternatives considered**: runtime phase check only (#304 mutex, shipping), single-thread render-thread (rejected — loses queue-level parallelism), builder pattern, etc. With explicit reasons each was rejected.
- **Open questions** flagged for review.

## Verification

- Doc is reviewed and approved before any type-introducing code (#321) lands.
- Every current `GpuContext` method appears in the API-split table.

## Parent

#319
