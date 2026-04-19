---
whoami: amos
name: Introduce GpuContextSandbox + GpuContextFullAccess newtype wrappers
status: completed
description: Add the two capability types as thin newtype wrappers around `GpuContext`. Both expose the same full API initially — pure compile-time change with no behavioral impact.
github_issue: 321
dependencies:
  - "down:Design doc: GpuContextSandbox + GpuContextFullAccess API surface"
adapters:
  github: builtin
---

@github:tatolab/streamlib#321

## Branch

`refactor/gpu-capability-types` from `main`.

## Steps

1. Add `GpuContextSandbox` and `GpuContextFullAccess` in `libs/streamlib/src/core/context/gpu_context.rs`. Each holds an `Arc<GpuContext>` (or equivalent) internally.
2. Implement every existing `GpuContext` method on both, delegating to the inner context. No method hidden or removed yet.
3. Provide `pub(crate)` or marker-based conversion between the two — full enforcement comes in #324.
4. No call-site changes in this task — existing code keeps using `GpuContext` directly.

## Verification

- `cargo check` clean, no observable API changes.
- `cargo test -p streamlib` passes.
- Doc-comment lines make clear which type is which in IntelliSense.

## Parent

#319
