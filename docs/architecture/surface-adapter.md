# Surface adapters

Surface adapters are how StreamLib hands a host-allocated GPU surface
to a customer in their framework's idiomatic shape — Vulkan, OpenGL,
Skia, CPU readback, custom RHI — without ever exposing DMA-BUF fds,
DRM modifiers, or timeline semaphores.

This doc is the customer-facing brief. Adapter authors should also
read [`docs/architecture/adapter-authoring.md`](adapter-authoring.md)
for the implementation contract.

## The two-layer shape

Surface sharing is split into:

1. **Backing** — host-owned. A `VkImage` allocated with a
   render-target-capable DRM modifier on Linux. Owned by the
   StreamLib runtime; refcounted host-side.
2. **Per-API representation** — what the customer sees. Obtained by
   calling `acquire_read` / `acquire_write` on a `SurfaceAdapter`.
   The adapter takes the host backing and hands back the framework's
   idiomatic handle: a `VkImage`+`VkImageLayout`, a `GLuint` texture
   id, an `SkSurface`, a `&[u8]` slice — whatever the customer's
   framework wants.

The same backing can be wrapped by different adapters at different
times (or sequentially, never simultaneously). A surface's lifetime
is tied to the backing; the adapter is just a per-acquire view.

This shape is borrowed from production systems that converged on the
same answer: Chromium `SharedImage` + `SharedImageRepresentation`,
Dawn `SharedTextureMemory::BeginAccess` / `EndAccess`, Skia
`GrBackend*`, Unreal `FExternalTextureRegistry`.

## Why scope hides synchronization

The customer never types the word "semaphore." They write:

```rust
{
    let mut guard = adapter.acquire_write(&surface)?;
    let view = guard.view_mut();
    // ... draw into the view ...
}
// guard.drop() releases the access
```

Inside `acquire_write` the adapter waits on the host's acquire-side
timeline-semaphore value (so a previous reader/writer's GPU work has
finished). At guard drop, the adapter signals the release-side value
(so the next consumer's acquire wakes). Layout transitions
(`UNDEFINED → COLOR_ATTACHMENT_OPTIMAL`, etc.) live inside the same
scope. None of this surfaces in the customer's API.

In Python the same shape uses the language's idiomatic scope
binding:

```python
with ctx.gpu_limited_access.acquire_pixel_buffer(width, height) as surface:
    surface.lock(read_only=False)
```

> A "Deno scope binding" example was removed here: the TypeScript `using
> guard = adapter.acquireWrite(surface)` snippet — the Deno SDK and its
> native cdylib are gone, and Python is the only authoring runtime.

### Blocking vs. non-blocking acquire

The Rust trait exposes both flavors:

- `acquire_read` / `acquire_write` — blocks until the timeline
  semaphore wait completes (and, for write, until any contended
  reader/writer releases). Right shape for batch consumers.
- `try_acquire_read` / `try_acquire_write` — returns
  `Ok(None)` immediately when the surface is contended, never blocks.
  Right shape for streamlib processor-graph nodes that must not stall
  their thread runner waiting for a downstream consumer.

The conformance suite exercises both — passing it means an adapter
implements them correctly.

## Composition via capability markers

Outer adapters compose on inner adapters via marker traits. The basic
`VulkanWritable` covers callers that only need to issue Vulkan
commands against the image:

```rust
pub trait VulkanWritable {
    fn vk_image(&self) -> VkImageHandle;
    fn vk_image_layout(&self) -> VkImageLayoutValue;
}
```

`vk_image_layout()` is a deliberate escape hatch — many Vulkan-on-Vulkan
compositions need the current layout to insert layout-transition
barriers. Customers of `SurfaceAdapter` itself never see it; only
adapter authors composing on Vulkan do.

Frameworks that need a richer description of the underlying `VkImage`
(Skia's `GrVkImageInfo`, debug snapshotting, serialization) require the
extended marker `VulkanImageInfoExt`, which returns a `#[repr(C)]
VkImageInfo` struct carrying format / tiling / usage / sample-count /
level-count / queue-family / memory-binding / ycbcr-conversion plus
reserved bytes for additive ABI extensions:

```rust
impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for SkiaSurfaceAdapter<D> {
    type ReadView<'g> = SkiaReadView<'g, D>;
    type WriteView<'g> = SkiaWriteView<'g, D>;
    // build_skia_image_info::<V: VulkanImageInfoExt> fills the entire GrVkImageInfo.
}
```

The customer of `SkiaSurfaceAdapter` only ever sees `SkSurface`. The inner
view is a private detail of the outer adapter.

Other capability markers:
- `GlWritable` — view exposes `gl_texture_id() -> u32`.
- `CpuReadable` — view exposes `read_bytes() -> &[u8]`.
- `CpuWritable` — view exposes `write_bytes() -> &mut [u8]`.

## Concurrency

Several `acquire_read` calls on the same surface are permitted
concurrently — readers don't conflict. `acquire_write` is exclusive:
it fails with `AdapterError::WriteContended` if any reader or writer
is currently holding the surface.

This mirrors `RwLock`. The typestate (separate `acquire_read` and
`acquire_write` methods returning distinct guard types) makes
"acquired-read but tried to write" a compile error rather than a
runtime error.

## Subprocess lifetime

Helper processes hold a `StreamlibSurface` whose transport handle
carries the DMA-BUF fds checked out over the per-runtime Unix socket.
A clean release travels back over that socket as a `release` (a.k.a.
`unregister`) request framed by `streamlib-surface-client`. When a
helper crashes mid-write,
the kernel closes the per-subprocess Unix socket; the host's
surface-share watchdog observes the disconnect (kernel-side
equivalent of `EPOLLHUP`) and releases every surface registered
under that subprocess's `runtime_id`. The double-release case is
idempotent — a polite `release` followed by a crash leaves nothing
for the watchdog to do.

A helper process builds its own `VkInstance` + `VkDevice` through
`ConsumerVulkanDevice` — it never holds a reference to the host's
logical device, which is what makes the capability boundary
type-enforced. The two devices target the same physical GPU without
tripping the NVIDIA dual-`VkDevice` crash (see
[`docs/learnings/nvidia-dual-vulkan-device-crash.md`](../learnings/nvidia-dual-vulkan-device-crash.md)),
which is a same-process failure and stays out of reach while the
carve-out has the consumer submitting only at acquire/release
boundaries. Vulkan on the helper side is import-side only:
FD-imported memory via `VkImportMemoryFdInfoKHR` is the sole legal
allocation path, and everything privileged escalates to the parent.

> An "ABI version gate" section was removed here:
> `STREAMLIB_ADAPTER_ABI_VERSION` and the `streamlib-plugin-abi`
> `PluginDeclaration` precedent — the constant exists nowhere in the tree
> and the plugin-ABI crate was deleted with the plugin cdylibs.

## Where the code lives

- `adapters/streamlib-surface-adapter/` — the contract crate. Trait,
  descriptor, errors, guards, mock, conformance suite, subprocess
  crash harness.
> A "Python mirror" bullet was removed here: `sdk/streamlib-
> python/python/streamlib/surface_adapter.py` — that SDK path is gone (it is
> `sdk/streamlib-python-wheel/` now) and the wheel ships no
> `surface_adapter` module.
- `runtime/streamlib-engine/src/linux/surface_share/` — host-side backing
  store and the Unix-socket service that hands DMA-BUF fds to
  subprocesses.

In-tree adapter implementations live in their own crates
(`streamlib-adapter-vulkan`, `-opengl`, `-skia`, `-cpu-readback`,
`-cuda`).
