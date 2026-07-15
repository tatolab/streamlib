# Adapter runtime integration

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation):
> use Opus, show your work, treat this academically, not dogmatically.

## Overview

A subprocess customer (Python, Deno) obtains a usable
`VulkanContext` / `OpenGlContext` / `SkiaContext` /
`CpuReadbackContext` / `CudaContext` instance against StreamLib's
host-side surface adapters via two IPC seams plus the
`streamlib-consumer-rhi` carve-out — without re-implementing host
RHI patterns and without breaking the `LimitedAccess` /
`FullAccess` capability typestate split in
`runtime/streamlib-engine/src/core/context/`.

A Python customer writing

```python
with skia_adapter.acquire_write(surface) as guard:
    sk_surface = guard.view
    # draw stuff
```

works the same way it works in-process Rust — same trait shape,
same resource semantics, same scope-bound synchronization.

## The two IPC seams

Both seams are wired through `GpuContextLimitedAccess` so subprocess
code never crosses into `FullAccess`:

### Seam 1 — surface-share registry

`runtime/streamlib-engine/src/linux/surface_share/` plus client at
`runtime/streamlib-surface-client/src/linux.rs`. One-shot
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

Already used by `examples/polyglot-dma-buf-consumer/runner/` from both
Python and Deno via `ctx.gpu_limited_access.resolve_surface(id)`,
which under the hood does a `check_out` and hands back a handle
the subprocess can `lock` / read / `unlock` / `release`.

### Seam 2 — escalate IPC

`runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_escalate.rs`,
typed by JTD schemas at
`packages/escalate/schemas/escalate_{request,response}.yaml` (the
`@tatolab/escalate` peer protocol package).
Length-prefixed JSON request/response over the subprocess's
stdin/stdout pipes, with a discriminator-tagged op enum covering
the surface-acquire ops (`AcquireImage`, `AcquirePixelBuffer`,
`AcquireTexture`), `Log`, `ReleaseHandle`, the cpu-readback
trigger (`RunCpuReadbackCopy`, `TryRunCpuReadbackCopy`), the
compute / graphics / ray-tracing register + run ops, and
`RegisterAccelerationStructureBlas` / `Tlas`. See
`escalate_request.yaml` for the canonical list.

Each request carries a UUID `request_id`; responses echo it.
Adding a new op is a schema change → `cargo xtask generate-schemas`
→ rebuild all three runtimes (Rust, Python, Deno). The host side
holds resources alive on behalf of the subprocess via
`EscalateHandleRegistry`; subprocess crash drops them.

The acquire-style ops (`AcquireImage`, etc.) are how the
surface-share registry gets populated in the first place — host
allocates a backing, registers it under a UUID, returns the UUID
to the subprocess, which then `check_out`s the FDs from
surface-share.

## The single-pattern shape

Every surface adapter rides the same shape, per the engine-model
principle in CLAUDE.md ("the RHI is the single gateway"):
pre-register resources via surface-share, import them through
`consumer-rhi`, run the adapter generic over
`D: VulkanRhiDevice`. Per-acquire IPC, when host work is needed
(cpu-readback's copy, escalated compute / graphics / ray-tracing
dispatch), is a thin trigger that publishes a timeline value the
consumer waits on through the carve-out — not a fresh FD-passing
payload.

Concretely:

| Adapter | Pattern (single shape) |
|---|---|
| `streamlib-adapter-vulkan` | Generic over `D: VulkanRhiDevice`. Host pre-registers `VkImage` + two timeline semaphores (`produce_done` + `consume_done`) via surface-share; subprocess imports through `ConsumerVulkanTexture` + a pair of `ConsumerVulkanTimelineSemaphore`s. Per-acquire is layout-transition + `produce_done` wait, no IPC. The writer process signals `produce_done` in `end_write_access`; the reader process signals `consume_done` in `end_read_access`. Both edges typically use host CPU `signal_host`. See [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md) for the single-writer-per-edge contract. |
| `streamlib-adapter-opengl` | Same shape; subprocess imports the `VkImage` and binds it as a `GL_TEXTURE_2D` via EGL DMA-BUF import. (Has not yet lifted to the dual-timeline shape — see [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md); will migrate in a separate issue.) |
| `streamlib-adapter-skia` | Same shape; composes on the vulkan adapter's import path (and also offers a GL backend that composes on the opengl adapter). (Has not yet lifted to the dual-timeline shape — see [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md); will migrate in a separate issue.) |
| `streamlib-adapter-cpu-readback` | Same shape: host pre-registers a HOST_VISIBLE staging `VkBuffer` + two timeline semaphores (`produce_done` + `consume_done`) via surface-share; subprocess imports through `ConsumerVulkanBuffer` + a pair of `ConsumerVulkanTimelineSemaphore`s. Per-acquire is a thin `RunCpuReadbackCopy(surface_id)` IPC that triggers the host's `vkCmdCopyImageToBuffer` and signals `produce_done` via the trigger's `vkQueueSubmit2::pSignalSemaphoreInfos` slot; the subprocess waits on `produce_done`, mmaps the pre-imported staging buffer, then signals `consume_done` via CPU `signal_host` in `end_read_access`. See [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md) for the single-writer-per-edge contract. |
| `streamlib-adapter-cuda` | Same shape with one twist on the FD wire — two resource flavors: **(a)** the flat-tensor DLPack path: host pre-registers a HOST_VISIBLE OPAQUE_FD-exportable `VkBuffer` (`HostVulkanBuffer::new_opaque_fd_export`) + two OPAQUE_FD-exportable timeline semaphores (`produce_done` + `consume_done`) via surface-share; subprocess imports through `ConsumerVulkanBuffer::from_opaque_fd` + a pair of `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`, then maps the same FDs into CUDA via `cudaImportExternalMemory(OPAQUE_FD)` + `cudaImportExternalSemaphore(TimelineSemaphoreFd)`. The OPAQUE_FD handle type (rather than DMA-BUF) is forced by the DLPack zero-copy contract: PyTorch / JAX / NumPy `from_dlpack` consume `DLTensor.data` as a flat `void*`, and only `cudaExternalMemoryGetMappedBuffer` (which requires the source memory to be a `VkBuffer` exported as OPAQUE_FD) yields the flat pointer. **(b)** the tiled-image path: host pre-registers a DEVICE_LOCAL OPAQUE_FD-exportable `VkImage` (`HostVulkanTexture::new_opaque_fd_export`) — `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier, format restricted to the CUDA-mappable subset (`Rgba8Unorm` / `Rgba16Float` / `Rgba32Float`) — and the subprocess imports through `ConsumerVulkanTexture::from_opaque_fd`. The same FD is then handed to CUDA via `cudaImportExternalMemory(OPAQUE_FD)` → `cudaExternalMemoryGetMappedMipmappedArray` for `cudaSurfaceObject_t` / `cudaTextureObject_t` backings. The mipmapped-array handle is opaque (not DLPack-compatible) but unlocks hardware-bilinear sampling, mipmap LOD selection, and surface-write writes from CUDA kernels — the texture-shaped slice that complements (a)'s flat-tensor slice. The host-side trigger that produces frames into the staging buffer signals `produce_done` via `vkQueueSubmit2::pSignalSemaphoreInfos`; the subprocess waits on `produce_done` before consuming and signals `consume_done` via CPU `signal_host` in `end_read_access`. No per-acquire IPC, no CUDA bridge trait. See [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md) for the single-writer-per-edge contract. |

All adapters follow this shape — no outliers.

## Customer-facing surface

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
request IDs. That's the whole point of the adapter pattern.

## Layered architecture

```
Customer code (Rust processor / Python script / Deno script)
  └── adapter.acquire_write(surface)              ← public API, uniform
      └── streamlib-{python,deno} adapter Protocol ← type stub
          └── streamlib-{python,deno}-native FFI   ← runtime impl
              └── streamlib-adapter-* (vulkan, opengl, skia,
                                       cpu-readback, cuda)
                  ↳ generic over D: VulkanRhiDevice
                  ↳ pre-registered resources via surface-share
                  ↳ imports via streamlib-consumer-rhi (Consumer*)
                  ↳ per-acquire: layout transitions + timeline waits;
                    thin escalate-IPC trigger when host work needed
                    (cpu-readback's vkCmdCopyImageToBuffer; escalated
                    compute / graphics / ray-tracing dispatch)
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
   `.claude/rules/polyglot.md`)
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
`LimitedAccess`, sends a `run_cpu_readback_copy` request with a
`surface_id`. Host receives it on a worker holding `FullAccess`,
runs `vkCmdCopyImageToBuffer` on the host VkDevice + queue
(queue mutex, fence pool, submit instrumentation all covered)
into the staging buffer that was pre-registered via surface-share
at setup time, and returns the `produce_done` timeline value to
wait on. The subprocess waits on the imported `produce_done`
timeline through the carve-out, reads the pre-imported staging
buffer it already mmapped, then signals `consume_done` from
`end_read_access`.

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

## Runtime wiring — `install_setup_hook`

Every surface adapter's host-side wiring runs through
[`Runner::install_setup_hook`][hook]. The hook fires exactly
once per `start()`, after `GpuContext::init_for_platform_sync` has
created the live `GpuContext` but before any processor's `setup()`
runs — the window where adapter bridges and pre-allocated host
surfaces have to be in place.

[hook]: ../../runtime/streamlib-engine/src/core/runtime/runtime.rs

The shape of what the hook does varies by seam:

- **Surface-share seam** (Vulkan, OpenGL, Skia). The hook allocates
  the host's `Texture` (via
  `gpu.acquire_render_target_dma_buf_image` for render-target-capable
  DMA-BUF), allocates the per-edge timelines (typically each via
  `HostVulkanTimelineSemaphore::new_exportable`), registers them in
  surface-share with a known UUID via
  `gpu.surface_store().register_texture(uuid, &texture,
  Some(produce_done.as_ref()), Some(consume_done.as_ref()),
  current_image_layout)` — `produce_done` and `consume_done` are the
  two `&HostVulkanTimelineSemaphore` handles (writer/reader directions
  per [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)),
  `current_image_layout` is the producer's post-write `VulkanLayout`
  consumed by Path 2 QFOT acquire — and stashes any per-runtime
  sync state the adapter needs (timeline semaphores, DRM modifier
  records). When the same surface flows downstream to an
  **in-process** Rust consumer on the hot path, the hook also calls
  `gpu.register_texture_with_layout(uuid, texture.clone(), layout)`
  — see [Dual-registration for in-process
  consumers](#dual-registration-for-in-process-consumers) below. No
  bridge — every subprocess acquire is a one-shot `check_out`.
  (Vulkan rides the dual-timeline shape today; OpenGL and Skia
  haven't lifted yet and will migrate in a separate issue.)
- **Escalate-IPC seam** (cpu-readback). The hook constructs the
  `CpuReadbackSurfaceAdapter`, allocates + registers the host
  surface(s) it serves (passing both `produce_done` and `consume_done`
  through `register_pixel_buffer_with_timeline`), and registers a
  `CpuReadbackBridge` implementation on the GpuContext via
  `gpu.set_cpu_readback_bridge(...)`. The bridge is the dispatch
  target the escalate handler reaches when a subprocess sends
  `run_cpu_readback_copy`; the trigger signals `produce_done`, the
  subprocess signals `consume_done` in `end_read_access`.
- **Surface-share seam with OPAQUE_FD** (cuda). The hook allocates a
  HOST_VISIBLE OPAQUE_FD-exportable `VkBuffer` via
  `HostVulkanBuffer::new_opaque_fd_export` (rather than
  `acquire_render_target_dma_buf_image`) plus two OPAQUE_FD-exportable
  timelines (`produce_done` + `consume_done`, each via
  `HostVulkanTimelineSemaphore::new_exportable`), registers them
  through the same surface-share API with
  `RhiExternalHandle::OpaqueFd { fd, size }` so the wire format
  carries `handle_type: "opaque_fd"`. No bridge; the cdylib does the
  CUDA-side work (`cudaImportExternalMemory(OPAQUE_FD)` →
  `cudaExternalMemoryGetMappedBuffer` → DLPack capsule
  construction) entirely inside its own `cudarc` integration. The
  host-pipeline side, when it needs to write into the staging
  buffer per frame, runs `vkCmdCopyImageToBuffer` as a normal
  pipeline step authored by whoever wired the runtime — not by the
  adapter — and that submit signals `produce_done`. The cdylib
  signals `consume_done` from `end_read_access`.

The compute / graphics / ray-tracing kernel bridges follow the same
shape (`gpu.set_compute_kernel_bridge`, `set_graphics_kernel_bridge`,
`set_ray_tracing_kernel_bridge`) for adapters that escalate kernel
dispatch through the host RHI.

Reference implementation:
`examples/polyglot-cpu-readback-blur/runner/src/main.rs`. That example shows
the cpu-readback case (which exercises the bridge path); the GPU
adapters use the same hook but skip the `set_*_bridge` step.

The hook is the canonical opt-in registration point. Adapters that
need pre-start GpuContext access use it; adapters that need
per-acquire host work also expose a `set_*_bridge` setter on
`GpuContext` mirroring `set_cpu_readback_bridge`. Application
authors call `install_setup_hook` exactly once per adapter they want
to expose to subprocesses.

### Dual-registration for in-process consumers

`gpu.surface_store().register_texture(...)` publishes the surface
to the cross-process surface-share daemon. It does NOT populate
`GpuContext`'s in-process `texture_cache`. Same-process consumers
calling `gpu.resolve_texture_registration_by_surface_id(...)`
therefore miss Path 1 (`texture_cache` HashMap lookup) and fall
through to Path 2 (`surface_store.lookup_texture` + DMA-BUF FD
import + QFOT acquire submit per call). Path 2 explicitly does NOT
cache its synthesized `TextureRegistration` — it reimports per-call
by design — so an in-process consumer reading the same surface
every frame would pay a fresh import + QFOT acquire on every
render.

For hot-path in-process consumers (e.g. `LinuxDisplayProcessor`,
the `BlendingCompositor`, video encoders), populate Path 1 by
**dual-registering** in the setup hook:

```rust
// 1. Cross-process publish (subprocess customers):
gpu.surface_store()
    .ok_or(...)?
    .register_texture(
        SCENARIO_SURFACE_UUID,
        &texture,
        Some(produce_done.as_ref()),
        Some(consume_done.as_ref()),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    )?;

// 2. In-process Path 1 fast path (same-process consumers):
gpu.register_texture_with_layout(
    &SCENARIO_SURFACE_UUID,
    texture.clone(),
    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
);
```

`produce_done` and `consume_done` are typically each allocated via
`HostVulkanTimelineSemaphore::new_exportable`; the writer/reader
contract is documented in
[`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md).

Both calls take the same `current_layout`. The producer is
responsible for keeping the two declarations consistent — one is
read by Path 2 (cross-process, via `surface_store.lookup_texture`),
the other by Path 1 (in-process, via the registry held in
`GpuContext`). Lying in either is the
[`TextureRegistration` anti-pattern #2 — descriptor-side claims
that don't match registration](texture-registration.md#anti-patterns).

The reference in-tree producer is `LinuxCameraProcessor` in the
`streamlib-camera` package — `packages/camera/src/linux/camera.rs`
calls both `store.register_texture(...)` and
`gpu_context.register_texture_with_layout(...)` (outside the
`escalate(|full| ...)` closure where the ring textures were
constructed) for every ring texture it allocates, with the same
`VulkanLayout::SHADER_READ_ONLY_OPTIMAL` declaration on both
sides.

#### When the second call is unnecessary

When the surface is consumed **only** by subprocess customers (or
by a post-stop one-shot like `gpu.create_texture_readback`), the
in-process call is redundant — Path 1 is never consulted. The
canonical example is
[`examples/polyglot-opengl-fragment-shader/runner/src/main.rs`](../../examples/polyglot-opengl-fragment-shader/runner/src/main.rs):
the host registers via `surface_store.register_texture` only and
relies on `gpu.create_texture_readback` for its post-stop pixel
capture. Don't dual-register surfaces with no in-process hot-path
consumer — every entry in `texture_cache` lives until the
producer explicitly unregisters, and over-populating it muddies
the cache's purpose (per-surface lifecycle state for in-process
fast-path resolution; see
[`texture-registration.md`](texture-registration.md#what-goes-in--what-stays-out)).

#### Why not auto-couple the two registrations

Considered and rejected: making `surface_store.register_texture`
populate `texture_cache` automatically. Two reasons it doesn't fit:

- The two registries have different scopes by design.
  `texture_cache` is for in-process consumers reaching textures
  via Path 1; `surface_store` is for cross-process consumers
  reaching them via the surface-share daemon. Auto-coupling them
  re-introduces the stale-content risk when adapters re-register
  the same UUID — `texture_cache` would silently rebind to a
  different texture, while `Path 2` resolves through a separate
  IPC roundtrip that already encodes per-call freshness.
- The doc-only path is the conservative fix. The dual-registration
  call is one line per setup hook; the engine-level coupling is a
  layered API change with non-local consequences.

If a future adapter genuinely needs both registrations to stay in
lock-step (e.g. a producer that re-registers on every layout
transition), revisit — but the current shape is two explicit
calls.

### Trade-off — explicit registration vs. Cargo-feature ambient availability

With `install_setup_hook` the model is:

1. Add the adapter crate as a Cargo dep.
2. Call `runtime.install_setup_hook(...)` exactly once at app
   startup, doing the adapter's required pre-start work (allocate
   host surfaces, register in surface-share, set bridge if needed).

The cost: one extra line of wiring per adapter at the application's
`main.rs`. Compile-time presence is not enough — you have to
explicitly hand the adapter the resources it manages. Embedded /
headless deployments that just want "everything that compiled in to
be available" pay a real, if small, ergonomic cost here.

What that cost buys:

- **Explicit and greppable.** `git grep install_setup_hook` tells
  you exactly which adapters this runtime exposes to subprocesses
  and what host surfaces it pre-allocates. No ambient surprises
  ("wait, why is cpu-readback available, I didn't enable it?").
- **Lifetime control.** The hook captures the adapter `Arc`, so the
  application owns when the adapter is destroyed. A Cargo feature
  can't express lifetime — it'd either leak per-process state for
  the whole binary's life, or hand-roll a separate teardown path.
- **Per-runtime configuration.** Multiple `Runner` instances
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

A per-adapter `install_default` convenience helper (e.g.
`streamlib_adapter_cpu_readback::install_default(&runtime, surface_size)`)
that internally calls `install_setup_hook` with sensible defaults
is a clean opt-in escape from the boilerplate; the underlying
explicit API stays as the explicit form. Don't replace explicit
registration with implicit feature-flag discovery — the auditability
property is load-bearing.

## Related

- `docs/architecture/surface-adapter.md` — the customer-facing
  brief for the adapter trait shape
- `docs/architecture/adapter-authoring.md` — implementation
  contract for new surface adapters (checklist, crate skeleton,
  trip-wires, hypothetical walkthrough)
- `docs/architecture/adapter-timeline-single-writer.md` —
  single-writer-per-edge contract for the `produce_done` +
  `consume_done` timeline pair every subprocess-wired adapter
  registers with surface-share
- `docs/architecture/subprocess-rhi-parity.md` — how the
  subprocess obtains a usable RHI surface beyond the import-side
  carve-out (the integration-shape view of how the carve-out
  works alongside this doc's adapter-runtime-shape view)
- `.claude/rules/polyglot.md` — the polyglot rule, including
  the import-side carve-out
- `docs/architecture/adapter-authoring.md` — the adapter
  implementation contract
- `docs/learnings/nvidia-egl-dmabuf-render-target.md` —
  modifier-vs-`external_only` constraint that must be solved on
  host
- `docs/learnings/nvidia-dual-vulkan-device-crash.md` — why
  subprocess Vulkan code stays consumer-only
