---
whoami: amos
name: '@github:tatolab/streamlib#319'
adapters:
  github: builtin
description: 'GPU capability-based access (sandbox + escalate) — Umbrella — replace runtime-phase checks with compile-time capability types. `process()` receives `GpuContextLimitedAccess`; escalation to `GpuContextFullAccess` goes through a closure that reuses the setup mutex from #304. Setup becomes "compiler pre-escalates on behalf of the processor." Huge DX and correctness win; unblocks dynamic reconfigure.'
github_issue: 319
blocked_by:
- '@github:tatolab/streamlib#320'
- '@github:tatolab/streamlib#321'
- '@github:tatolab/streamlib#322'
- '@github:tatolab/streamlib#323'
- '@github:tatolab/streamlib#324'
- '@github:tatolab/streamlib#325'
- '@github:tatolab/streamlib#369'
- '@github:tatolab/streamlib#370'
- '@github:tatolab/streamlib#326'
---

@github:tatolab/streamlib#319

## Why

The #304 setup barrier enforces "resource creation is serialized" at runtime with a mutex. This umbrella is the compile-time half of the same invariant: the wrong method isn't in scope during `process()`, so the bad program can't be written. Every future processor, codec integration, and polyglot binding benefits; every current "3am `acquire_texture()` in a hot loop" bug becomes a compile error.

## Shape

- `process()` receives `&GpuContextLimitedAccess` — pool-backed acquires, sampling, writes to pre-reserved buffers. No heavy-allocation methods in scope.
- `sandbox.escalate(|full: &GpuContextFullAccess| { … })` is the single primitive for resource creation. It acquires the setup mutex (reused from #304), runs the closure, `wait_device_idle`, releases.
- `setup()` receives `&GpuContextFullAccess` — equivalent to the compiler calling `escalate()` on the processor's behalf before invoking setup.
- Reconfigure (mid-run resolution change, codec swap) is just a running processor calling `escalate()` itself. Same queue, same guarantees.
- Polyglot subprocesses inherently only have a sandbox (can't reach the host Vulkan device). IPC becomes their escalate channel, serialized on the host.

## Children (order)

1. #320 — Design doc (gates everything downstream)
2. #321 — Introduce the two newtypes (compile-only, identical API)
3. #322 — Migrate processor trait signatures
4. #323 — Implement `escalate()` primitive; compiler phase 4 becomes a consumer
5. #324 — Restrict Sandbox's surface (enforcement lands here; compile errors surface)
6. #325 — Polyglot IPC: escalate-on-behalf (MVP: `acquire_pixel_buffer` + `release_handle`)
7. #369 — Polyglot escalate: `acquire_texture` op (follow-up to #325)
8. #370 — xtask: JTD discriminator support so #325/#369 wire-types drop the hand-authored copy
9. #326 — Learning doc (captures the final shape, after #369 + #370 settle the polyglot wire)

## Dependencies / impact

- Depends on #304 (setup mutex is the primitive `escalate` reuses).
- Blocks #310 (pipeline resolution propagation — reconfigure wants this).
- Peer to #294 (driver-visible Vulkan cleanup). Orthogonal; either can land first.
- Scope: 2–4 weeks. Task #320 (design doc) is the gating deliverable.
