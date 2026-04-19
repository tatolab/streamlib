---
whoami: amos
name: Implement sandbox.escalate() reusing the setup mutex
status: pending
description: Add `GpuContextLimitedAccess::escalate(|full| …)` as the single primitive for all GPU resource-creation work. Internally acquires the mutex from #304, hands the closure a `FullAccess`, waits for idle on exit, releases.
github_issue: 323
dependencies:
  - "down:Migrate processor trait signatures to capability-aware setup/process"
adapters:
  github: builtin
---

@github:tatolab/streamlib#323

## Branch

`feat/sandbox-escalate-primitive` from `main`.

## Steps

1. Add `GpuContextLimitedAccess::escalate<F, T>(&self, f: F) -> Result<T> where F: FnOnce(&GpuContextFullAccess) -> Result<T>` in `libs/streamlib/src/core/context/gpu_context.rs`. Grab `processor_setup_lock`, construct a `FullAccess` scoped to the closure, run `wait_device_idle` on exit, release.
2. Rewrite `libs/streamlib/src/core/compiler/compiler_ops/spawn_processor_op.rs` Phase 4 so it calls `sandbox.escalate(|full| invoke_setup(full))` instead of the bespoke manual lock-grab shipped in #304. The mutex and wait_idle become private implementation details of `escalate`.
3. Expose enough RuntimeContext plumbing for a running processor to call `escalate()` itself (mid-run reconfigure path).

## Verification

- Re-run the #304 h265 20× loop (`/dev/video2`); zero `DEVICE_LOST`.
- Unit test: multiple threads concurrently call `escalate`; each closure sees exclusive access (verifiable via a shared counter / sleep).
- `device_wait_idle` fires exactly once per escalation.
- Dynamic graph add/remove still works.

## Parent

#319
