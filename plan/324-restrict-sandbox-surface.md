---
whoami: amos
name: Restrict GpuContextSandbox API surface to safe ops
status: pending
description: The enforcement task. Remove every heavy-allocation method from `GpuContextSandbox`. Process() bodies that call privileged methods become compile errors; fix each by pre-reserving in setup() or wrapping in `escalate()`.
github_issue: 324
dependencies:
  - "down:Implement sandbox.escalate() reusing the setup mutex"
adapters:
  github: builtin
---

@github:tatolab/streamlib#324

## Branch

`feat/sandbox-restrict-surface` from `main`.

## Steps

1. Per the API-split table from #320, strip `GpuContextSandbox`'s impl down to pool acquires (pre-reserved blocks only), texture sampling, writes to mapped pixel buffers, and read-only queries.
2. Fix every resulting compile error in `process()` bodies: either move the call into an `escalate(|full| …)` closure, or pre-reserve the resource in `setup()` and have `process()` reuse it.
3. Ensure pool-growth paths internally call `escalate()`. Sandbox callers must never observe a growth allocation that bypasses serialization.
4. Audit `acquire_pixel_buffer` / `acquire_texture` carefully — fast path on Sandbox, slow/growth path goes through `escalate`.
5. Debug-build only: add a counter that warns if a single processor calls `escalate` more than N times per second (signals misuse — escalation should be rare).

## Verification

- `cargo build -p streamlib` fails loudly if old unrestricted API is called from `process()` anywhere in the tree (there should be none by the end of this task).
- E2E roundtrip (vivid + Cam Link) per `docs/testing.md`; zero `DEVICE_LOST`, zero `OUT_OF_DEVICE_MEMORY`.
- Spot-check via the new counter: steady-state `process()` fires zero escalations.

## Parent

#319
