---
whoami: amos
name: Migrate processor lifecycles to RuntimeContextFullAccess / RuntimeContextLimitedAccess
status: completed
description: Flip every processor lifecycle method (setup / teardown / on_pause / on_resume / process, plus Manual mode start / stop) to receive a capability-typed ctx parameter. Setup / teardown / start / stop get RuntimeContextFullAccess; on_pause / on_resume / process get RuntimeContextLimitedAccess. Stashed Option<GpuContext…> fields disappear. Also renames GpuContextSandbox → GpuContextLimitedAccess for naming consistency (FullAccess / LimitedAccess as a coherent pair).
github_issue: 322
dependencies:
  - "down:Introduce GpuContextLimitedAccess + GpuContextFullAccess newtype wrappers"
adapters:
  github: builtin
---

@github:tatolab/streamlib#322

## Design revision (2026-04-19)

Initial scope targeted #320 §8.Q1 **Option A** — keep trait signatures unchanged, processors stash `Option<GpuContextLimitedAccess>`, privileged setup-time calls reach `ctx.full_access().expect(…)`. Shipped as PR #349 draft.

Review feedback rejected Option A on DX grounds: processors still had to *remember* to stash the sandbox, *remember* to not stash FullAccess, and the compiler couldn't catch wrong-phase usage. That contradicts streamlib's philosophy of making it easy to do the right thing and hard to do the wrong thing.

Revised scope adopts **Option B** *and extends it to every lifecycle method*, not just `process()`. The per-call typed ctx parameter forces correct usage at every call site; LSP autocomplete guides the author to the right API; no stashing required.

## Branch

`refactor/processor-capability-signatures` from `main` (force-push-resets PR #349 to the new approach).

## Naming

Rename `GpuContextSandbox` → `GpuContextLimitedAccess` everywhere as a prerequisite commit. Reasons:
- `FullAccess` / `LimitedAccess` is a proper axis pair — both describe the kind of access granted.
- `Sandbox` / `FullAccess` mixed metaphors (containment vs. authority).
- Applying "name-encodes-role" naming standard (CLAUDE.md) to both halves.

## New trait shape

```rust
pub trait ReactiveProcessor {
    async fn setup    (&mut self, ctx: &RuntimeContextFullAccess)    -> Result<()>;
    async fn teardown (&mut self, ctx: &RuntimeContextFullAccess)    -> Result<()>;
    async fn on_pause (&mut self, ctx: &RuntimeContextLimitedAccess) -> Result<()>;
    async fn on_resume(&mut self, ctx: &RuntimeContextLimitedAccess) -> Result<()>;
    fn       process  (&mut self, ctx: &RuntimeContextLimitedAccess) -> Result<()>;
}
```

Same shape for `ContinuousProcessor`. `ManualProcessor` adds `start(&mut self, ctx: &RuntimeContextFullAccess)` and `stop(&mut self, ctx: &RuntimeContextFullAccess)` — resource-lifecycle methods get full access.

`RuntimeContextFullAccess` exposes `.gpu_full_access() -> &GpuContextFullAccess`; `RuntimeContextLimitedAccess` exposes `.gpu_limited_access() -> &GpuContextLimitedAccess`. Both wrap the shared `RuntimeContext` for non-GPU fields (time, runtime_id, tokio_handle, etc.).

## Enforcement

- Both `RuntimeContextFullAccess` and `RuntimeContextLimitedAccess` are `!Clone` and `!Send` — the lifetime of the borrow is strictly the call.
- Passed by reference `&'a …` — a processor cannot stash `&ctx` in a field (borrow checker).
- `GpuContextFullAccess` is already `!Clone` (landed in earlier 508d1bf attempt, preserved).
- The old `RuntimeContext::gpu` / `sandbox()` / `full_access()` accessors are deleted — no escape hatch.
- `compile_fail` doc tests lock in `!Clone`, `!Send`, and "`gpu_full_access()` does not exist on limited ctx".

## Staged commit plan

Each commit compiles + passes the suite, so `git bisect` works.

1. **Rename** `GpuContextSandbox` → `GpuContextLimitedAccess` everywhere. Mechanical; zero semantic change.
2. **Introduce** `RuntimeContextFullAccess` / `RuntimeContextLimitedAccess` newtypes (wrap `RuntimeContext`), nothing consumes them yet.
3. **Plumb typed ctx through runtime**: attribute macro codegen (`__generated_setup` / `_teardown` / `_on_pause` / `_on_resume` / process), `spawn_processor_op`, `thread_runner`, `run_processor_loop` — each call site builds the right ctx.
4. **Flip trait signatures** on `ReactiveProcessor` / `ContinuousProcessor` / `ManualProcessor`. Wide compile errors → fix every processor inline.
5. **Remove stashed fields**: delete `Option<GpuContextLimitedAccess>` / `Option<GpuContext>` from every processor struct; process bodies use the ctx parameter.
6. **Delete old accessors**: remove `RuntimeContext::sandbox()`, `::full_access()`, `::gpu_context_for_runtime()`. The old escape hatch is gone.

## Verification

- `cargo check` / `cargo test` clean after each commit.
- `compile_fail` doc tests assert `RuntimeContextFullAccess: Clone` / `Send` / `RuntimeContextLimitedAccess: Clone` / `Send` all fail, and that `gpu_full_access()` on a limited ctx fails.
- E2E matrix per `docs/testing.md`:
  - vivid h264 roundtrip
  - vivid h265 roundtrip
  - Cam Link 4K h264 roundtrip
  - camera-display-only (no codec)
- Deno / Python polyglot stubs: subprocess host Rust side compiles; polyglot SDK migration is tracked as #350 / #351 (not blocking this PR).

## Parent

#319

## Follows

- #350 — Deno polyglot SDK migration
- #351 — Python polyglot SDK migration
