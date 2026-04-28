# Adapter runtime integration

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation):
> use Opus, show your work, preserve disagreed-with content with
> reasoning rather than silently deleting. Treat this academically,
> not dogmatically.
>
> **2026-04-28 — Architectural correction.** Earlier revisions of
> this doc recommended a "hybrid" shape: GPU adapters (Vulkan /
> OpenGL / Skia) ride the surface-share seam, cpu-readback rides
> escalate IPC. That bucketing was wrong-shaped. **Every surface
> adapter rides the same single-pattern shape**: pre-registered
> resources via surface-share + `consumer-rhi` import, plus thin
> per-acquire IPC triggers when the host has work to do. See
> [Single-pattern principle](../../docs/architecture/subprocess-rhi-parity.md#single-pattern-principle-2026-04-28)
> in `subprocess-rhi-parity.md` and the cpu-readback rewire (Path E)
> issue under milestone *Surface Adapter Architecture*. The
> recommendation section below is preserved with crossed-out content
> per the markdown-editing rules so future readers can see the
> dead-end and why.

## Question

How does a subprocess customer (Python, Deno, future others) obtain
a usable `VulkanContext` / `OpenGlContext` / `SkiaContext` /
`CpuReadbackContext` instance against StreamLib's host-side surface
adapters, without re-implementing host RHI patterns and without
breaking the [`LimitedAccess` / `FullAccess` capability
typestate](../../libs/streamlib/src/core/context/)?

## Context

After the Surface Adapter Architecture milestone shipped four
adapter crates — `streamlib-adapter-vulkan` (#511),
`streamlib-adapter-opengl` (#512), `streamlib-adapter-skia` (#513,
host crate still in flight), `streamlib-adapter-cpu-readback`
(#514) — the customer-facing trait is in place but no subprocess
runtime constructs a usable instance. The polyglot wrappers in
`streamlib-python` and `streamlib-deno` carry only Protocol /
interface type stubs.

The bar this design must clear (paraphrased from the PR #527 review
of #514): *"the milestone is complete only if a downstream issue
can use the adapter from day one."* That means a Python customer
writing

```python
with skia_adapter.acquire_write(surface) as guard:
    sk_surface = guard.view
    # draw stuff
```

must work the same way it works in-process Rust — same trait shape,
same resource semantics, same scope-bound synchronization.

## What's already shipped (so the doc isn't speculating)

Two IPC seams already exist in tree, both wired through
`GpuContextLimitedAccess` so subprocess code never crosses into
`FullAccess`:

### Seam 1 — surface-share registry

`libs/streamlib/src/linux/surface_share/` plus client at
`libs/streamlib-surface-client/src/linux.rs`. One-shot
length-prefixed JSON request/response over Unix socket with
`SCM_RIGHTS` ancillary FD passing. Operations:

| op | direction | payload |
|---|---|---|
| `register` / `check_in` | client → host | metadata + FDs (one per plane) |
| `lookup` / `check_out` | client → host | `surface_id` → metadata + FDs |
| `unregister` / `release` | client → host | `surface_id` (idempotent) |

Metadata travels alongside FDs: `width`, `height`, `format`,
`plane_sizes`, `plane_offsets`, `plane_strides`, `drm_format_modifier`,
`resource_type` (`pixel_buffer` | `texture`). The wire format is
extensible — additional JSON fields can be added without breaking
existing clients.

Already used by `examples/polyglot-dma-buf-consumer/` from both
Python and Deno via `ctx.gpu_limited_access.resolve_surface(id)`,
which under the hood does a `check_out` and hands back a handle
the subprocess can `lock` / read / `unlock` / `release`.

### Seam 2 — escalate IPC

`libs/streamlib/src/core/compiler/compiler_ops/subprocess_escalate.rs`,
typed by JTD schemas at
`libs/streamlib/schemas/com.streamlib.escalate_{request,response}@1.0.0.yaml`.
Length-prefixed JSON request/response over the subprocess's
stdin/stdout pipes, with discriminator-tagged op enum:

```rust
pub enum EscalateRequest {
    AcquireImage(EscalateRequestAcquireImage),
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),
    AcquireTexture(EscalateRequestAcquireTexture),
    Log(EscalateRequestLog),
    ReleaseHandle(EscalateRequestReleaseHandle),
}
```

Each request carries a UUID `request_id`; responses echo it.
Adding a new op is a schema change → `cargo xtask generate-schemas`
→ rebuild all three runtimes (Rust, Python, Deno). The host side
holds resources alive on behalf of the subprocess via
`EscalateHandleRegistry`; subprocess crash drops them.

The current acquire-style ops (`AcquireImage`, etc.) are how the
surface-share registry gets populated in the first place — host
allocates a backing, registers it under a UUID, returns the UUID
to the subprocess, which then `check_out`s the FDs from
surface-share.

## Three architectural directions considered

### Option A — escalate IPC op per adapter

Subprocess JSON-RPCs the host on every `acquire_*` call. Host
runs the adapter's `acquire_*` against its in-tree implementation,
blocks on the per-surface timeline, and returns the framework-native
handle metadata (or, for cpu-readback, a freshly-populated staging
FD).

- **Pros** — All synchronization, layout transitions, and
  adapter-specific state stays on the host. Bug fixes land once.
  Polyglot SDKs stay tiny. Host's queue mutex / fence pool /
  submit instrumentation cover every dispatch.
- **Cons** — IPC roundtrip latency on every acquire. Per-adapter
  JTD schema regen + 3-runtime rebuild. The escalate seam wasn't
  designed for hot-path acquire/release traffic.

### Option B — surface-share registry extension

Extend the surface-share registry so a registered surface also
carries an "adapter handle" entry. Subprocess SDK looks up the
entry and constructs the right `*Context` from the looked-up data.
cpu-readback's staging buffer becomes a separately-registered
surface in the same registry; vulkan/opengl/skia just use the
existing FD.

- **Pros** — Reuses the `polyglot-dma-buf-consumer` plumbing.
  One IPC seam, not per-op. No per-acquire roundtrip for GPU
  adapters.
- **Cons** — cpu-readback semantically wants a host-driven copy
  on every `acquire_read` (`vkCmdCopyImageToBuffer`), not a
  one-shot FD handoff. Forcing it through the registry means
  re-registering on every acquire — that's an escalate op in
  registry clothing.

### Option C — hybrid

GPU adapters (Vulkan, OpenGL, Skia) ride the surface-share
registry path: host pre-allocates the backing with the right
DRM modifier (NVIDIA EGL trap solved once, on host); subprocess
does a one-shot FD lookup and wraps it as the framework-native
handle. cpu-readback rides the escalate-IPC path: each
`acquire_read` is a JSON-RPC ping that triggers the host's
`vkCmdCopyImageToBuffer`, after which the subprocess mmaps the
freshly-populated staging FD.

- **Pros** — Each adapter takes the seam that matches its
  data-flow shape. No per-acquire IPC roundtrip for GPU adapters.
  Host-driven copy semantics preserved for cpu-readback.
- **Cons** — Two integration paths to understand instead of one.
  Future adapter authors must consciously pick which seam fits.

## Recommendation

> ~~**Option C — hybrid.** GPU adapters ride surface-share;
> cpu-readback rides escalate IPC.~~ — **Superseded 2026-04-28.**
> The hybrid framing was an architectural drift: it conflated
> "host has per-acquire work" (true for cpu-readback's copy) with
> "host must per-acquire-pass FDs back to the subprocess" (false —
> staging buffers + timeline can be pre-registered through the
> same surface-share seam vulkan/opengl use). Earlier reviews
> didn't separate those concerns.
>
> The actual rule, per the engine-model principle in CLAUDE.md
> ("the RHI is the single gateway"): **every surface adapter
> rides the same shape**. Pre-register resources via surface-share,
> import them through `consumer-rhi`, run the adapter generic over
> `D: VulkanRhiDevice`. Per-acquire IPC, when host work is needed
> (cpu-readback's copy, escalated compute via #550), is a thin
> trigger that publishes a timeline value the consumer waits on
> through the carve-out — not a fresh FD-passing payload.
>
> Concretely:

| Adapter | Pattern (single shape) |
|---|---|
| `streamlib-adapter-vulkan` | Generic over `D: VulkanRhiDevice`. Host pre-registers `VkImage` + timeline via surface-share; subprocess imports through `ConsumerVulkanTexture` + `ConsumerVulkanTimelineSemaphore`. Per-acquire is layout-transition + timeline wait, no IPC. |
| `streamlib-adapter-opengl` | Same shape; subprocess imports the `VkImage` and binds it as a `GL_TEXTURE_2D` via EGL DMA-BUF import. |
| `streamlib-adapter-skia` | Same shape; composes on the vulkan adapter's import path. |
| `streamlib-adapter-cpu-readback` | Same shape: host pre-registers a HOST_VISIBLE staging `VkBuffer` + a timeline semaphore via surface-share; subprocess imports through `ConsumerVulkanPixelBuffer` + `ConsumerVulkanTimelineSemaphore`. Per-acquire is a thin `RunCpuReadbackCopy(surface_id)` IPC that triggers the host's `vkCmdCopyImageToBuffer` and returns the timeline value to wait on. Subprocess waits on the imported timeline through the carve-out, then mmaps the pre-imported staging buffer. |

`vulkan/opengl/skia` adapters already follow this shape after #560
Phase 2. `cpu-readback`'s rewire is the cpu-readback rewire issue
under milestone #16.

## Customer-facing surface — unchanged

The seam choice is **internal to the adapter implementation**.
From the customer's perspective the API is identical regardless
of where they're running and which seam the runtime picks:

```rust
// Rust, in-process
let mut guard = adapter.acquire_write(&surface)?;
let view = guard.view_mut();
```

```python
# Python subprocess
with adapter.acquire_write(surface) as guard:
    view = guard.view
```

```typescript
// Deno subprocess
{
  using guard = adapter.acquireWrite(surface);
  const view = guard.view;
}
```

The customer never sees DMA-BUF FDs, DRM modifiers, timeline
semaphores, queue family ownership transitions, or escalate
request IDs. That's the whole point of the adapter pattern, and
this design preserves it.

## Layered architecture

```
Customer code (Rust processor / Python script / Deno script)
  └── adapter.acquire_write(surface)              ← public API, unchanged
      └── streamlib-{python,deno} adapter Protocol ← type stub
          └── streamlib-{python,deno}-native FFI   ← runtime impl
              └── streamlib-adapter-* (vulkan, opengl, skia, cpu-readback)
                  ↳ generic over D: VulkanRhiDevice
                  ↳ pre-registered resources via surface-share
                  ↳ imports via streamlib-consumer-rhi (Consumer*)
                  ↳ per-acquire: layout transitions + timeline waits;
                    thin escalate-IPC trigger when host work needed
                    (cpu-readback's vkCmdCopyImageToBuffer, escalated
                    compute via #550)
                      └── host RHI                ← FullAccess, only here
```

Above the host-RHI line the customer sees a single uniform shape.

## Capability sandbox preservation

The `RuntimeContextLimitedAccess` / `RuntimeContextFullAccess`
typestate (and the parallel `GpuContextLimitedAccess` /
`GpuContextFullAccess` split) exist to make a class of bugs
unreachable at compile time: subprocess code cannot accidentally
allocate exportable memory, cannot configure modifiers, cannot
construct compute pipelines. The two seams preserve this
guarantee differently:

### Surface-share path (Vulkan / OpenGL / Skia)

The privileged operation — allocating a `VkImage` with a
render-target-capable DRM modifier — happens on the host at
backing creation time, far upstream of the customer's `acquire_*`
call. By the time the subprocess does its `check_out` lookup,
the FD is already an artifact; what the subprocess does with it
is bounded:

1. `VkImportMemoryFdInfoKHR` (the import-side carve-out from
   `.claude/workflows/polyglot.md`)
2. `vkBindImageMemory` / `vkBindBufferMemory`
3. Layout transitions + sync wait/signal on imported handles
4. Render or compute against the imported handle

None of those touch `vkAllocateMemory`, no modifier discovery,
no pool management. The subprocess `VkDevice` (consumer-only by
construction) holds the imported handles; the host `VkDevice`
(`FullAccess`) is never directly referenced from subprocess
address space.

### Escalate-IPC path (cpu-readback)

The crossing **is** the IPC wire. Subprocess holds
`LimitedAccess`, sends a JSON-RPC request with a `surface_id`
and `mode=read`. Host receives it on a worker holding
`FullAccess`, runs `vkCmdCopyImageToBuffer` on the host VkDevice
+ queue (queue mutex, fence pool, submit instrumentation all
covered), exports the resulting staging-buffer FD via
surface-share, returns the FD reference. Subprocess mmaps and
reads bytes.

There is no in-process `LimitedAccess → FullAccess` upgrade ever.
The typestate split is enforced by the IPC boundary itself, the
same way it's enforced for every other escalate op.

### Customer's view

`adapter.acquire_write(surface)` — at the API surface — is a
`LimitedAccess` operation in subprocess code and a `FullAccess`
operation in in-process Rust code. The trait shape is identical;
the underlying typestate is whichever the surrounding context
carries. A customer writing processor code never sees `FullAccess`
unless their processor itself is wired to receive it.

## Trip-wires for future adapters

If any of the following becomes true for a new adapter, revisit
this design — the working hypothesis may not fit:

1. **Subprocess wants to allocate.** A new adapter that needs a
   subprocess-side staging buffer larger than what import + bind
   covers would break the import-side carve-out. Escalate the
   allocation to host instead.
2. **Subprocess wants its own `VkPipeline` / compute kernel.**
   That re-introduces the SPIR-V reflection / descriptor-set
   layout / pipeline cache problems
   `core::rhi::ComputeKernelDescriptor` solved once on host. If
   a future adapter needs subprocess-local compute, prefer
   escalating the dispatch as a single "run kernel K with bindings
   B" op rather than building a parallel kernel cache.
3. **Per-acquire host work for what looks like a GPU adapter.**
   If a Vulkan/OpenGL adapter discovers it needs fresh host work
   on every `acquire_*` (e.g. dynamic format negotiation), it's
   probably better routed as an escalate op even though it's a
   GPU adapter. The bucketing isn't framework-agnostic — it
   tracks data-flow shape.
4. **Hot-path acquire/release.** If an escalate-IPC adapter
   starts being called at frame rate and the JSON-RPC roundtrip
   shows up in profiles, the answer is probably to batch
   acquires (one escalate op covering N frames) before reaching
   for shared memory or a third seam.

## Open questions for the user

- **Hot-path cpu-readback.** If a future scenario reads back
  every frame at 60fps, is per-acquire JSON-RPC acceptable, or
  do we want a "subscribe to readbacks" escalate op that
  populates a shared-memory ring? Filing as a follow-up if/when
  it becomes load-bearing; not blocking the initial
  cpu-readback runtime.
- **Skia (#513) is still in flight.** This doc specifies the
  seam Skia should ride (surface-share, transitively via
  Vulkan). If #513's host-side implementation lands with a
  different shape, this section needs updating. Marked as a
  trip-wire above.

## Runtime wiring — `install_setup_hook`

> Added 2026-04-27 (#529). All adapter integrations register their
> host-side state through this single API.

Every surface adapter's host-side wiring runs through
[`StreamRuntime::install_setup_hook`][hook]. The hook fires exactly
once per `start()`, after `GpuContext::init_for_platform_sync` has
created the live `GpuContext` but before any processor's `setup()`
runs — the window where adapter bridges and pre-allocated host
surfaces have to be in place.

[hook]: ../../libs/streamlib/src/core/runtime/runtime.rs

The shape of what the hook does varies by seam:

- **Surface-share seam** (Vulkan, OpenGL, Skia). The hook allocates
  the host's `StreamTexture` (via
  `gpu.acquire_render_target_dma_buf_image` for render-target-capable
  DMA-BUF), registers it in surface-share with a known UUID via
  `gpu.surface_store().register_texture(uuid, &texture)`, and stashes
  any per-runtime sync state the adapter needs (timeline semaphores,
  DRM modifier records). No bridge — every subprocess acquire is a
  one-shot `check_out`.
- **Escalate-IPC seam** (cpu-readback). The hook constructs the
  `CpuReadbackSurfaceAdapter`, allocates + registers the host
  surface(s) it serves, and registers a `CpuReadbackBridge`
  implementation on the GpuContext via
  `gpu.set_cpu_readback_bridge(...)`. The bridge is the dispatch
  target the escalate handler reaches when a subprocess sends
  `acquire_cpu_readback`.

Reference implementation:
`examples/polyglot-cpu-readback-blur/src/main.rs`. That example shows
the cpu-readback case (which exercises the bridge path); the GPU
adapters use the same hook but skip the `set_*_bridge` step.

The hook is the canonical opt-in registration point. Future adapters
that need pre-start GpuContext access should use it; adapters that
need per-acquire host work should also expose a `set_*_bridge` setter
on `GpuContext` mirroring `set_cpu_readback_bridge`. Application
authors call `install_setup_hook` exactly once per adapter they want
to expose to subprocesses.

### Trade-off — explicit registration vs. Cargo-feature ambient availability

The pre-#529 mental model was implicit: a Cargo feature like
`streamlib/adapter-cpu-readback` would compile the adapter in and
the runtime would discover it ambiently (via `inventory` registration
or similar). That's not how this works anymore. With
`install_setup_hook` the model is:

1. Add the adapter crate as a Cargo dep.
2. Call `runtime.install_setup_hook(...)` exactly once at app
   startup, doing the adapter's required pre-start work (allocate
   host surfaces, register in surface-share, set bridge if needed).

The cost: one extra line of wiring per adapter at the application's
`main.rs`. Compile-time presence is no longer enough — you have to
explicitly hand the adapter the resources it manages. Embedded /
headless deployments that just want "everything that compiled in to
be available" pay a real, if small, ergonomic cost here.

What we get for that cost (and why it was the right call):

- **Explicit and greppable.** `git grep install_setup_hook` tells
  you exactly which adapters this runtime exposes to subprocesses
  and what host surfaces it pre-allocates. No ambient surprises
  ("wait, why is cpu-readback available, I didn't enable it?").
- **Lifetime control.** The hook captures the adapter `Arc`, so the
  application owns when the adapter is destroyed. A Cargo feature
  can't express lifetime — it'd either leak per-process state for
  the whole binary's life, or hand-roll a separate teardown path.
- **Per-runtime configuration.** Multiple `StreamRuntime` instances
  in the same process can wire different adapter sets, or wire the
  same adapter against different surface dimensions / DRM modifiers
  / quality knobs. Cargo features are per-binary; this is per-runtime.
- **No magic about required setup.** Every adapter has *some*
  pre-start work — at minimum allocating one or more host surfaces
  and registering them. A Cargo feature flag can't do that work; it
  can only flip a compile-time bit. The hook makes the work the
  application has to do for that adapter visible at the call site,
  next to the surface-allocation arguments.
- **Type safety on bridge wiring.** `gpu.set_cpu_readback_bridge(...)`
  takes a typed `Arc<dyn CpuReadbackBridge>` — wrong-type bridges
  are a compile error. A feature-flag-driven registration would
  funnel everything through a generic registry and lose that.

When this trade-off becomes painful and what to do about it: if
applications start writing the same five-line adapter setup
boilerplate over and over, the right answer is a per-adapter
`install_default` convenience helper (e.g.
`streamlib_adapter_cpu_readback::install_default(&runtime, surface_size)`)
that internally calls `install_setup_hook` with sensible defaults.
The convenience helper is opt-in and additive; the underlying
explicit API stays as the escape hatch. Don't replace explicit
registration with implicit feature-flag discovery — the auditability
property is load-bearing.

## Implementation issues

The subprocess runtimes for the three already-shipped adapters all
flow through the single-pattern shape post-#560:

- ~~`#529` — `feat(adapter-cpu-readback): subprocess
  CpuReadbackContext runtime + cv2 fixture` — escalate IPC seam~~
  — Closed under the dual-seam framing. The cpu-readback rewire
  issue under milestone #16 supersedes this: cpu-readback joins
  the unified shape (pre-registered staging + timeline via
  surface-share, thin per-acquire copy trigger).
- `#530` — `feat(adapter-opengl): subprocess OpenGlContext
  runtime + scenario binary` — single-pattern shape, lives in
  consumer-rhi.
- `#531` — `feat(adapter-vulkan): subprocess VulkanContext
  runtime + scenario binary` — single-pattern shape, lives in
  consumer-rhi.

Suggested implementation order: cpu-readback first (smallest
data shape; escalate-IPC seam is well-trodden), opengl second
(DRM-modifier import path already exists in
`polyglot-dma-buf-consumer`), vulkan third (most plumbing,
biggest blast radius if any of the others surfaces a design
gap).

`#513` (Skia host crate) is not yet implemented; its subprocess
runtime issue should be filed once #513 lands and inherits this
doc's recommendation.

## Related

- `docs/architecture/surface-adapter.md` — the customer-facing
  brief for the adapter trait shape
- `.claude/workflows/polyglot.md` — the polyglot rule, including
  the import-side carve-out
- `docs/learnings/nvidia-egl-dmabuf-render-target.md` —
  modifier-vs-`external_only` constraint that must be solved on
  host
- `docs/learnings/nvidia-dual-vulkan-device-crash.md` — why
  subprocess Vulkan code stays consumer-only
- `#525` — separate research on subprocess-side RHI pattern
  parity (escalate vs per-language). Orthogonal to this doc:
  #525 is about *implementation parity* (does the subprocess
  reimplement RHI patterns), this doc is about *integration
  shape* (how does the subprocess obtain a usable adapter
  context).
