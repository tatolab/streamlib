# Cdylib reachability

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

How to expose host-side functionality to cdylib-resident processor
code without tripping a `host_inner()` panic guard, a same-thread
escalate-gate deadlock-panic, or worse.

The document is structured around the five distinct patterns that
have shipped in this codebase to bridge cdylib → host reaches. Each
applies to a different class of "I'm in cdylib code and I need X."
Picking the wrong one is the historical failure class: agents see a
`host_inner()` panic, reach for the closest precedent without
checking the lifecycle stage or the type shape, and ship a fix that
either (a) works but introduces a new latent deadlock, (b) works for
one case but doesn't generalize, or (c) silently breaks a sibling
in-process registration path. This doc exists so the next agent
picking up "cdylib can't reach X" has a single decision tree, not
five disconnected precedents.

## Why this doc exists

Two known traps:

1. **Constructor-bridge trap.** Seeing a `host_inner()` panic on one
   `Host*` PluginAbiObject (e.g., `Texture::host_inner()`) and assuming by
   analogy that every other `Host*` constructor on the engine needs a
   bridge. That assumption is wrong by default — most `Host*`
   constructors take only `&Arc<HostVulkanDevice>` plus primitives
   and touch only `pub` accessors on the device. Workspace plugin
   cdylibs that hold the device Arc (via the v9 bridge) can call
   them directly, no new ABI surface needed.

2. **Workaround-pattern trap.** Seeing a `host_inner()` panic
   somewhere on the cdylib path and reaching for the nearest
   precedent (an `escalate(|full| ...)` wrap, a fresh Arc transit,
   an underscore reach-through) without first asking "which of the
   established patterns does this case belong in?" The historical
   failure mode: three processors shipped a
   `gpu_limited_access().escalate(|full| ...)`-from-setup
   workaround for the panic-class #1072 fixed, recreating a
   well-known deadlock the sandbox's `processor_setup_lock` has
   forbidden since pre-#322. Nobody on the review side caught it
   because no doc existed naming the patterns.

This doc captures the full decision tree across every cdylib-reach
pattern, the per-pattern enforcement layer, and the explicit ban on
the workarounds that look like fixes but aren't — so a future agent
picking up "the cdylib can't reach X" lands on the right pattern by
checklist rather than precedent-hunting.

## Decision tree — pick a pattern

```
Cdylib code (or a workspace plugin's processor body) needs to call
something on the host side.

  ┌──────────────────────────────────────────────────────────────┐
  │ Q1: WHICH lifecycle context type is the body running under?  │
  └─────────────────┬────────────────────────────────────────────┘
                    │
       ┌────────────┴────────────────────┐
       │                                 │
   FullAccess context:               LimitedAccess context:
   setup() / teardown() /            process() / on_pause() /
   Manual-mode start() / stop()      on_resume() /
   (&RuntimeContextFullAccess)       Reactive start()
                                     (&RuntimeContextLimitedAccess)
       │                                 │
       ▼                                 ▼
   Pattern 1: direct                Q2: what kind of reach?
   ctx.gpu_full_access().X(),       (see below)
   no escalate. ALL FullAccess
   lifecycle methods are dispatched
   under `with_cdylib_scope` for
   cdylib-resident processors
   (#1072 + #1075), so the body
   sees a ScopeToken FullAccess
   that routes through the
   FullAccess vtable transparently.
   DO NOT call `.escalate(...)` —
   gate is held, same-thread
   re-entry panics with an
   actionable msg.

  ┌──────────────────────────────────────────────────────────────┐
  │ Q2: From a LimitedAccess context, what kind of reach?        │
  └─────────────────┬────────────────────────────────────────────┘
                    │
     ┌──────────────┼──────────────┬─────────────────────┐
     │              │              │                     │
  one-time     per-frame      need a              need a host-Arc
  privileged   hot-path       PluginAbiObject     (Arc<HostVulkanX>)
  setup        binding/work   binding (set_X
                              on compute kernel)
     │              │              │                     │
     ▼              ▼              ▼                     ▼
   Pattern 4     Pattern 5      Pattern 3             Pattern 2
   escalate +    per-method     per-method            PluginAbiObject
   FullAccess    vtable slot    vtable slot           Arc-transit slot
   work in       w/ raw         (set_storage_         (host_vulkan_
   closure       vulkanalia     buffer_pixel etc)    X_arc — #1066/
                 handle wire                          68/69/70/71)
                 (set_*_image_
                  view, record)
```

**Critical asymmetry**: Pattern 4 (escalate) only applies to
LimitedAccess contexts. Reaching for it from a FullAccess context
(setup/teardown/Manual-start/Manual-stop) is anti-pattern #1 — the
gate is already held by the engine's lifecycle wrap, and re-entry
panics at runtime. The xtask lint
(`xtask check-no-escalate-in-lifecycle`) flags this statically; the
runtime panic is the backstop.

The five patterns:

- **Pattern 1: lifecycle wrap** — `RuntimeContextFullAccess::with_cdylib_scope`
  wraps every FullAccess lifecycle method (`setup`, `teardown`,
  Manual-mode `start`, `stop`) for cdylib-resident processors,
  giving the body a fresh ScopeToken FullAccess (#1072 introduced
  it for setup/teardown; #1075 extended to start/stop for
  symmetry). The cdylib body calls `ctx.gpu_full_access().X()`
  directly; methods that have FullAccess vtable analogs dispatch
  through them. This restores the historical contract
  ("`FullAccess` in signature → direct access in body") for every
  runtime variant.
- **Pattern 2: PluginAbiObject Arc-transit vtable slot** —
  `host_vulkan_device_arc`, `host_vulkan_texture_arc`,
  `host_vulkan_pixel_buffer_arc`, etc. Hands cdylib an
  `Arc<HostVulkan*>` raw pointer it can call host-public accessors
  on. Use for accessor returns where the engine-internal type can
  cross the plugin ABI as an Arc handle (#1066/68/69/70/71).
- **Pattern 3: per-method vtable slot** — `set_storage_buffer_pixel`
  and siblings on `VulkanComputeKernel` (and the equivalent on
  `VulkanGraphicsKernel`, `VulkanRayTracingKernel`,
  `RhiCommandRecorder`). Each binding/dispatch method that takes
  engine-public PluginAbiObjects gets its own typed vtable slot with the
  same shape on both sides. Use for hot-path binding work where the
  method args ARE plugin-ABI-safe types.
- **Pattern 4: escalate to FullAccess** — `gpu_limited_access().escalate(|full| ...)`
  from a **LimitedAccess context** (`process()`, `on_pause()`,
  `on_resume()`, Reactive `start()`). The historical primitive for
  upgrading Limited → Full when you don't have FullAccess to start
  with. Acquires the escalate gate + waits device idle; serializes
  against other privileged work. **Never call from a FullAccess
  context** — the engine's lifecycle wrap (Pattern 1) already
  holds the gate, and re-entry panics with an actionable message
  via `EscalateGate`'s same-thread re-entry detector. The xtask
  lint `check-no-escalate-in-lifecycle` enforces this statically.
- **Pattern 5: per-method vtable slot carrying a raw vulkanalia
  handle** — `set_sampled_image_view`, `set_combined_image_sampler_view`,
  `set_storage_image_view`, and `record` on `VulkanComputeKernel`.
  Engine SDK code (`RgbToNv12Converter::convert`,
  `Nv12ToRgbConverter::convert`) compiled into workspace plugin
  cdylibs reaches these `pub(crate)` methods on the per-frame path.
  The vtable slot carries the raw `vk::ImageView` / `vk::CommandBuffer`
  as a `u64` wire value (vulkanalia's handle types are
  `#[repr(transparent)]` over `u64` / `usize`); the host wrapper
  reconstructs the typed handle via `Handle::from_raw` before
  forwarding. Same structural shape as Pattern 3 — the asymmetry
  is that the binding payload here is a raw vulkanalia integer
  rather than an engine-public PluginAbiObject. The host method stays
  `pub(crate)` because subprocess (Python/Deno) cdylibs don't
  link the engine SDK code that reaches it; only workspace plugin
  cdylibs (h264/h265/camera) hit this path. This pattern landed
  via v5 of `VulkanComputeKernelMethodsVTable`.

## Constructor bridge sub-decision (was Pattern 2's sub-tree)

If you're adding or evaluating a `Host*` Vulkan RHI primitive
constructor (`new` / `from_*` / `create_*`), the older sub-tree
applies — it predates the broader pattern catalog above and is
specifically about route-1 vs route-2 for constructor bodies. Keep
the original decision intact:

```
Workspace plugin cdylib needs Arc<HostX> (a new resource type, or a
new constructor on an existing type).

  ┌─────────────────────────────────────────────────────────┐
  │ Q1: Does the constructor / extractor body reach for     │
  │     `host_inner()` (PluginAbiObject deref) or           │
  │     `host_callbacks()` (cdylib-mode guard)?             │
  └───────────────┬─────────────────────────────────────────┘
                  │
        ┌─────────┴─────────┐
       no                  yes
        │                   │
        ▼                   ▼
   Route 2 works       Route 1 needed
   (direct call)       (new vtable slot)
```

## Route 2 — direct constructor call (preferred)

Applies when:

- The constructor takes only `&Arc<HostVulkanDevice>` (or other
  cdylib-reachable Arcs) and POD primitives.
- Its body uses only `pub` accessors on `HostVulkanDevice`
  (`allocator`, `device`, `dma_buf_buffer_pool`,
  `dma_buf_image_pool*`, `opaque_fd_buffer_pool*`,
  `opaque_fd_image_pool*`, `drm_modifier_table`, etc.) plus
  `vulkanalia-vma`.
- No `host_inner()` deref, no `host_callbacks()` check, no PluginAbiObject
  indirection.

Cdylib path:

```rust
gpu.escalate(|full| {
    let device_arc = full.host_vulkan_device_arc()?;
    // Direct constructor call — same code the host calls.
    let resource = HostVulkanXxx::new(&device_arc, ...args)?;
    let arc = Arc::new(resource);
    // ...use arc through the adapter...
    Ok(())
})
```

This is the cheapest path. No ABI surface ships; the cdylib-reachability
invariant is captured in:

- The type-level `# Cdylib reachability` docstring on each
  `Host*` type, enumerating the two paths and warning reviewers
  against introducing a guard inside the constructor body.
- `cargo xtask check-cdylib-reach` (see
  [Enforcement](#enforcement) below).
- The `load_project_dylib_*_smoke.rs` integration tests, which
  exercise the cdylib direct-call path against each surface adapter.

The existing route-2 paths today (verified at the docstring sites):

- `HostVulkanBuffer::new` / `::new_opaque_fd_export` / `::new_opaque_fd_export_device_local`.
- `HostVulkanTimelineSemaphore::new` / `::new_exportable` (plus
  the v6 `create_timeline_semaphore` slot for the non-exportable
  flavor when the cdylib doesn't already hold the device Arc).
- `HostVulkanTexture::new_render_target_dma_buf` (low-level
  path; the high-level `acquire_render_target_dma_buf_image`
  FullAccess slot is the recommended entry point for most adapters).

## Route 1 — new vtable slot (only when route 2 doesn't fit)

Applies when:

- The constructor / extractor must touch host-private state
  (`host_inner()` for PluginAbiObject deref, host-only registries, etc.).
- Or: the resource crosses the plugin ABI as a non-`Arc` opaque handle
  (PluginAbiObject with `(handle, vtable, POD)` layout, like `Texture` /
  `PixelBuffer` / `StorageBuffer`).

Existing route-1 bridges:

- **v9 `host_vulkan_device_arc`** — hands a cloned
  `Arc<HostVulkanDevice>` raw pointer to the cdylib.
- **v10 `host_vulkan_texture_arc`** — extracts an
  `Arc<HostVulkanTexture>` from a `Texture` PluginAbiObject (the PluginAbiObject's
  `host_inner()` panics in cdylib mode, hence the bridge).

When adding a new slot:

1. Bump `GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION`.
2. Update the layout regression test
   (`gpu_context_full_access_vtable_layout` in plugin-abi): assert
   the new `offset_of!` and the new `size_of`.
3. Update `host_services_layout_versions_pinned` with the bumped
   version constant.
4. Add at least one tier-1 wire-format test
   (`HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.<slot>(null) == null`).
5. Document the new version in the constant's docstring.

## Trap class

The mistake this doc exists to prevent: filing "bridge for X /
bridge for Y / bridge for Z" issues defensively without verifying
whether the constructor bodies actually need bridges. To the best
of our current knowledge, the historical case that motivated this
doc had three of four "bridges" proposed defensively turn out to
be already-working via route 2 — only one extractor (the
`Texture` PluginAbiObject's `host_inner()` deref) needed a real bridge.

Before filing a "bridge needed for X" issue:

1. **Read the constructor body** (and any internal helpers it
   delegates to). Does it reach for `host_inner` or
   `host_callbacks`?
2. If no — route 2 already works. The right deliverable is a
   docstring update + a smoke test, not a new vtable slot.
3. If yes — route 1 is needed, follow the checklist above.

## Anti-patterns (cdylib-reach failure modes that look like fixes)

These are the workarounds future agents tend to reach for instead
of picking the right pattern. Each is real — at least one shipped
in this codebase before being caught.

1. ❌ **`gpu_limited_access().escalate(|full| ...)` from inside
   `fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>)` or
   `fn teardown(...)`.** The lifecycle dispatch already holds the
   escalate gate around setup/teardown (via Pattern 1 for
   cdylib-resident processors, via `gpu_limited_access().escalate`
   for in-process VTable + LegacyDyn). Inner `.escalate(...)`
   re-enters the gate on the same thread; pre-#912 this silently
   deadlocked, post-#912 the gate has same-thread re-entry
   detection and panics with an actionable message
   ([`EscalateGate::enter`](../../libs/streamlib-engine/src/core/context/escalate_gate.rs)).
   **The right pattern is Pattern 1 — direct
   `ctx.gpu_full_access()` access from setup/teardown.**

2. ❌ **Reaching through private underscore-prefixed attributes**
   (`_lib`, `_handle_ptr`, `_internal_*`) on engine SDK types from
   example or test code. The presence of underscore means the
   field isn't part of the public surface — if you need it, the
   right deliverable is a public accessor + a docstring update,
   not a reach-through that future refactors will silently break.
   Same shape as the `_lib._handle_ptr` reach pattern the
   `feedback_examples_no_underscore_reachthrough` memory documents.

3. ❌ **`unsafe { &*(handle as *const SomeHostType) }` from cdylib
   code.** Cdylib code lives in a different plugin with a separately-
   compiled (potentially different rustc-version) view of
   `SomeHostType`'s layout. Even when the dlopen happens to share
   layout today, a routine `cargo update` on either side breaks
   it. The cdylib must dispatch via a typed vtable slot or hold an
   `Arc<HostX>` whose accessors are exposed publicly — see
   Patterns 2 and 3.

4. ❌ **Working around `host_inner()` panics by mirroring host-
   internal state on the cdylib side** (e.g., maintaining a
   parallel `HashMap<surface_id, Texture>` in cdylib code). The
   correct fix is to expose a vtable accessor on the host side
   (Pattern 2 or 3) — the parallel HashMap goes stale the moment
   the host's source-of-truth changes.

5. ❌ **Bypassing the panic guard via
   `kernel.host_inner().set_*(...)` to reach a `pub(crate)`
   method.** The `set_sampled_image_view` /
   `set_combined_image_sampler_view` / `set_storage_image_view` /
   `record` methods on `VulkanComputeKernel` are now mode-routed
   through the v5 `VulkanComputeKernelMethodsVTable` slots
   (Pattern 5 — see the catalog above). Reaching them via
   `host_inner()` instead skips the routing and reads host-private
   memory from the cdylib's address space — UB. Always call the
   public `pub(crate)` method (it picks the right path); never
   `host_inner()`.

## Enforcement

Three layers protect the invariant:

1. **`cargo xtask check-cdylib-reach`** — AST-level scan of every
   `.rs` file under `libs/streamlib-engine/src/vulkan/rhi/`. For
   each `impl HostVulkan*` block, walks every constructor-class
   method's body (`new*` / `create*` / `from_*`) and fails the
   build if any call expression references `host_inner` or
   `host_callbacks`. Adding a new `HostVulkan*` type / new file
   under the directory is covered automatically — the check
   tracks the directory, not a curated file list. Wired into CI
   as `check-cdylib-reach.yml`. Comments and string literals are
   ignored automatically by `syn`'s tokenization.
2. **Type-level docstrings** on `HostVulkanBuffer`,
   `HostVulkanTimelineSemaphore`, `HostVulkanTexture`. Each
   carries a `# Cdylib reachability` section enumerating the two
   paths and warning reviewers against introducing a guard. The
   docstring is what a reviewer sees first on a constructor
   change; the xtask is the backstop when the reviewer skipped it.
3. **Dlopen smoke tests** at
   `libs/streamlib-engine/tests/load_project_dylib_*_smoke.rs`
   (one per surface adapter: cpu-readback, vulkan, opengl, cuda).
   Each test exercises the cdylib direct-call path end-to-end.
   If a constructor regresses to host-private state, the matching
   smoke fails at runtime via `run_host_extern_c`'s panic-safety
   net surfacing the panic as `ERR:<msg>`. These tests are GPU-
   required and run locally only (no GPU CI runner planned per
   `project_ci_strategy_no_gpu`).

The three layers are defense in depth: the xtask catches the
syntactic regression before merge; the docstring deters the
regression at review time; the smoke tests catch any semantic
gap the xtask might miss.

## Reference

- `xtask/src/check_cdylib_reach.rs` — the AST scan + tests.
- `.github/workflows/check-cdylib-reach.yml` — CI wiring.
- `libs/streamlib-engine/src/vulkan/rhi/` — `# Cdylib reachability`
  docstrings on the `Host*` types that documented the route-2
  invariant (currently `HostVulkanBuffer`,
  `HostVulkanTimelineSemaphore`, `HostVulkanTexture`).
- `libs/streamlib-engine/tests/load_project_dylib_{cpu_readback,
  vulkan, opengl, cuda}_smoke.rs` — end-to-end exercise of the
  cdylib path.
