---
whoami: amos
name: Migrate processor trait signatures to capability-aware setup/process
status: pending
description: Flip `ReactiveProcessor` so `setup()` receives a `GpuContextFullAccess` handle and `process()` sees only a `GpuContextSandbox` via `RuntimeContext`. Purely mechanical; no behavior change.
github_issue: 322
dependencies:
  - "down:Introduce GpuContextSandbox + GpuContextFullAccess newtype wrappers"
adapters:
  github: builtin
---

@github:tatolab/streamlib#322

## Branch

`refactor/processor-capability-signatures` from `main`.

## Steps

1. Update `ReactiveProcessor` trait signatures in `libs/streamlib/src/core/processors/` — `setup(&mut self, ctx: RuntimeContext)` remains but `RuntimeContext` now carries a `FullAccess` handle internally; `process()` sees sandbox-only.
2. Expose `sandbox()` / `full_access()` accessors on `RuntimeContext` (or equivalent split); wire up in `libs/streamlib/src/core/context/runtime_context.rs`.
3. Update `libs/streamlib/src/core/compiler/compiler_ops/spawn_processor_op.rs` to hand the right handle into each phase.
4. Update every in-tree processor (camera, display, h264/h265 encoder+decoder, audio, MP4 writer, WebRTC, MoQ, CLAP host, subprocess hosts) to the new signatures. Identical API on both types at this point, so the changes are cosmetic.

## Verification

- `cargo check` / `cargo test` clean.
- Full E2E roundtrip (camera → encoder → decoder → display) per `docs/testing.md` passes for h264 + h265 on vivid.
- No observable runtime behavior change.

## Parent

#319
