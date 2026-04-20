---
whoami: amos
name: Restrict GpuContextLimitedAccess API surface to safe ops
status: completed
description: The enforcement task. Remove every heavy-allocation method from `GpuContextLimitedAccess`. Process() bodies that call privileged methods become compile errors; fix each by pre-reserving in setup() or wrapping in `escalate()`.
github_issue: 324
dependencies:
  - "down:Implement sandbox.escalate() reusing the setup mutex"
  - "down:Research: classify blit_copy cache-growth path (Sandbox vs Split)"
adapters:
  github: builtin
---

@github:tatolab/streamlib#324

## Branch

`feat/sandbox-restrict-surface` from `main`.

## Steps

1. Per the API-split table from #320, strip `GpuContextLimitedAccess`'s impl down to pool acquires (pre-reserved blocks only), texture sampling, writes to mapped pixel buffers, and read-only queries. **Per #320 §8.Q5**: `command_queue()` stays on Sandbox — the type-safety invariant is that Sandbox-reachable types can't compose into hostile payloads, so submitting pre-allocated buffers to the queue is safe.
2. Fix every resulting compile error in `process()` bodies: either move the call into an `escalate(|full| …)` closure, or pre-reserve the resource in `setup()` and have `process()` reuse it. **Per #320 §8.Q3**: NO transparent-escalate helpers (`acquire_*_or_escalate`). Pool-miss paths in Sandbox return an error; callers either handle it or wrap the call in an explicit `escalate()` closure. The closure boundary must be visible at every escalation site.
3. Ensure pool-growth paths internally call `escalate()`. Sandbox callers must never observe a growth allocation that bypasses serialization.
4. Audit `acquire_pixel_buffer` / `acquire_texture` carefully — fast path on Sandbox, slow/growth path goes through `escalate`.
5. **Per #320 §8.Q6**: debug-build escalation instrumentation. `tracing::trace!` on every `sandbox.escalate(…)` entry with processor ID, duration, and call-site stack (when the `tracing` backtrace feature is enabled). `tracing::warn!` on sustained >1 escalation/sec per processor. Record #304 mutex wait time in the same trace event. Release builds pay zero runtime cost. Steady-state `process()` must fire zero escalations — nonzero rate = processor needs more `setup()` pre-reservation.

## Verification

- `cargo build -p streamlib` fails loudly if old unrestricted API is called from `process()` anywhere in the tree (there should be none by the end of this task).
- E2E roundtrip (vivid + Cam Link) per `docs/testing.md`; zero `DEVICE_LOST`, zero `OUT_OF_DEVICE_MEMORY`.
- Spot-check via the new counter: steady-state `process()` fires zero escalations.

## Parent

#319
