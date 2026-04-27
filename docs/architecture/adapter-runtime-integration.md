# Adapter runtime integration

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation):
> use Opus, show your work, preserve disagreed-with content with
> reasoning rather than silently deleting. Treat this academically,
> not dogmatically. The recommendations below reflect the best
> understanding of the codebase as of 2026-04-27; trade-offs may
> shift as new adapters arrive or as existing seams evolve.

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

**Option C — hybrid.** The data-flow shapes are genuinely
different and forcing one seam on both buckets re-creates work
that already exists. Specifically:

| Adapter | Seam | Why |
|---|---|---|
| `streamlib-adapter-vulkan` | surface-share registry | Imported `VkImage`/`VkBuffer` is a static handle for the surface's lifetime. Once the FD is bound, every acquire is a layout-transition + sync wait — no fresh host work. |
| `streamlib-adapter-opengl` | surface-share registry | Same shape as Vulkan. The DRM-modifier import path already exists in `polyglot-dma-buf-consumer` and `nvidia-egl-dmabuf-render-target.md` documents the modifier-vs-`external_only` constraint that must be solved at allocation time on host. |
| `streamlib-adapter-skia` | surface-share registry | Skia composes on Vulkan via `VulkanImageInfoExt` (per `surface-adapter.md`), so it inherits Vulkan's seam transitively. |
| `streamlib-adapter-cpu-readback` | escalate IPC | Each `acquire_read` requires a fresh `vkCmdCopyImageToBuffer` on the host VkDevice/queue (the readback is a snapshot, not a static handle). The host-driven copy is exactly the kind of "small request, host does the privileged work" operation escalate IPC was built for. |

This recommendation is the working hypothesis; the trip-wires
section below names the conditions that would shift it.

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
              └── ONE OF:
                  ├── surface-share registry      ← Vulkan / OpenGL / Skia
                  └── escalate IPC                ← cpu-readback
                      └── host RHI                ← FullAccess, only here
```

Above the bottom line, the seam choice is invisible.

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

## Implementation issues

The subprocess runtimes for the three already-shipped adapters
inherit this design:

- `#529` — `feat(adapter-cpu-readback): subprocess
  CpuReadbackContext runtime + cv2 fixture` — escalate IPC seam
- `#530` — `feat(adapter-opengl): subprocess OpenGlContext
  runtime + scenario binary` — surface-share seam
- `#531` — `feat(adapter-vulkan): subprocess VulkanContext
  runtime + scenario binary` — surface-share seam

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
