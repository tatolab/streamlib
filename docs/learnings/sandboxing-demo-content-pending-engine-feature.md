# Sandboxing demo content out of engine pending a future engine feature

## When you need this

You're working on an example or scenario that needs hot-path GPU
work the engine doesn't expose a clean primitive for yet, AND the
right long-term home is a future engine feature that isn't built
(typically: render-graph / RDG #631, but the pattern generalizes).
The naïve options are both wrong:

- **Put the kernel/wrapper in the engine.** Encodes app-specific
  content (a demo's effects, a scenario's chrome) into the engine
  surface. Future agents reading the engine learn the wrong shape.
  This is what #487 had to *undo* — `VulkanBlendingCompositor` and
  `VulkanCrtFilmGrain` had been placed in `libs/streamlib/src/vulkan/rhi/`
  by #607/#608, and that placement actively encouraged copy-paste
  expansion of the same wrong shape.
- **Build the right engine primitive now.** Production engines (UE5,
  Bevy, Granite, wgpu) don't ship synchronous fence-blocked one-shot
  draw APIs as hot-path primitives — they use render graphs that
  schedule barriers across passes. Designing the engine primitive
  ahead of the use case (and ahead of RDG) is wrong-shape work that
  ages poorly.

The correct move is to **sandbox the demo content in the example
crate, gated by an explicit boundary-check exception, with a tracked
follow-up that removes the exception when the engine feature ships.**

## The recipe (three elements, all required)

### 1. Allowlist entry with explicit transitional rationale

Add the example crate to `xtask/src/check_boundaries.rs` allowlists
that the demo needs to bypass (typically `VULKANALIA_ALLOWLIST` and
`VULKANALIA_CARGO_DEP_ALLOWLIST`). The rationale comment **must**:

- Say `TRANSITIONAL` explicitly.
- Name the destination engine ticket (e.g. `RDG (#631)`).
- State the cleanup contract: the exception is removed in the same
  PR that lands the engine feature.

```rust
// camera-python-display (#487) — TRANSITIONAL exception. Removed
// when RDG (#631) ships and absorbs the kernel wrappers into
// render-graph passes.
AllowEntry {
    path: "examples/camera-python-display/",
    kind: AllowKind::PathPrefix,
    rationale: "transitional kernel-wrapper sandbox pending RDG (#631)",
},
```

Add a unit test that locks the exception in the same PR. A future
agent removing the allowlist also has to remove the test — that's a
deliberate act, not a slip.

### 2. Heavy module-level docs on the sandboxed code

Every file in the sandbox carries module-level prose explaining:

- **Why this lives here, not the engine.** Frame the wrong-shape
  pattern explicitly (synchronous fence-blocked dispatch, manual
  barrier management, app-specific content, etc.).
- **When it goes away.** Name the destination ticket. State that
  the boundary-check allowlist exception lifts at the same time.
- **What it does.** Lifecycle, contract, invariants — same as any
  load-bearing code.

The framing matters. Future agents reading sandboxed code as a
template for new code is the foot-gun this recipe exists to prevent
— the doc exists to short-circuit that reading.

### 3. Follow-up issue blocked by the destination ticket

File a follow-up issue with `Blocked by: #<destination>` that:

- Names the cleanup deliverables: remove the kernel wrappers,
  remove the direct vulkanalia imports, remove the allowlist
  entries, remove the locking unit tests.
- Includes acceptance criteria: `xtask check-boundaries` passes
  with the exception gone.

The follow-up ticket is the durable record. When the destination
ticket lands and unblocks the follow-up, the cleanup is mechanical.

## Survives vs throwaway

This recipe is **not** all throwaway. To the best of our current
knowledge, three artifacts survive the destination feature's arrival:

- **Shaders.** If the migration includes a shader port (compute →
  graphics, etc.), the shader payload survives unchanged. RDG /
  the future feature absorbs the *dispatch wrapper*, not the shader
  itself.
- **The dispatch-shape data.** A working sandbox is a tight
  specification for what the engine feature must absorb. "The
  wrappers do X, Y, Z" → "the engine feature must replace exactly
  X, Y, Z without regression."
- **Adjacent patterns.** Often the sandbox forces clarification of
  patterns that *were* engine-grade but underdocumented (e.g.
  dual-registration of output ring slots in the
  `texture_cache + surface_store` pattern). Those documentations
  outlive the sandbox.

What's throwaway:

- The kernel-wrapper structs themselves — replaced by the destination
  feature's primitives.
- The direct dependency on whatever low-level crate the boundary-check
  allowlist gated (e.g. `vulkanalia`) — replaced by the destination
  feature's public API.
- The allowlist entry + locking tests — removed when the imports go.

## Anti-patterns

These are the failure modes the recipe exists to prevent:

1. **Allowlisting without rationale.** Future agents see a bare
   path and assume it's permanent. Always include `TRANSITIONAL`
   + destination ticket.
2. **Heavy doc only at one site.** A doc on the wrapper file but no
   matching framing in `Cargo.toml` / `build.rs` / the boundary
   check leaves three out of four convergent signposts off — a
   future agent is more likely to land on the unsigned site.
3. **Skipping the follow-up.** Without the issue, the durable record
   is just code comments — easy for an agent unblocking the
   destination ticket to miss.
4. **Building the engine primitive opportunistically inside the
   sandbox.** The whole point is *not* to encode the wrong shape into
   the engine API. If you find yourself wanting to expose a
   primitive from the sandbox crate "for reuse" — stop. The
   destination ticket is the right scope for that work.
5. **Letting the sandbox grow indefinitely.** The recipe is for a
   bounded gap (one demo's content, awaiting one specific engine
   feature). If the sandbox is collecting unrelated content over
   time, the right move is to refile the destination ticket as
   higher priority, not to keep absorbing more demo work into the
   sandbox.

## Reference

- First in-tree application: #487 (camera-python-display kernel
  wrappers + shaders relocated from `libs/streamlib/src/vulkan/rhi/`
  into the example crate, gated for RDG #631).
- Cleanup follow-up: #689 (`Blocked by` #631).
- Boundary-check implementation: `xtask/src/check_boundaries.rs`
  (`VULKANALIA_ALLOWLIST` + `VULKANALIA_CARGO_DEP_ALLOWLIST`
  entries for `examples/camera-python-display/`, locking tests
  `allows_use_vulkanalia_in_camera_python_display_example` and
  `allows_vulkanalia_cargo_dep_in_camera_python_display_example`).
- Sandboxed wrappers (reference shape):
  `examples/camera-python-display/src/blending_compositor_kernel.rs`,
  `examples/camera-python-display/src/crt_film_grain_kernel.rs`.
