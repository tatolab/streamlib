# Surface adapters

Surface adapters are how StreamLib hands a host-allocated GPU surface
to a customer in their framework's idiomatic shape — Vulkan, OpenGL,
Skia, CPU readback, custom RHI — without ever exposing DMA-BUF fds,
DRM modifiers, or timeline semaphores.

This doc is the customer-facing brief. Adapter authors should also
read [`docs/adapter-authoring.md`](../adapter-authoring.md) for the
implementation contract.

## The two-layer shape

Surface sharing is split into:

1. **Backing** — host-owned. A `VkImage` allocated with a
   render-target-capable DRM modifier on Linux (or an `IOSurface` on
   macOS, in a future milestone). Owned by the StreamLib runtime;
   refcounted host-side.
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

In Python and Deno the same shape uses the language's idiomatic
scope binding:

```python
with adapter.acquire_write(surface) as view:
    view.draw_into(...)
```

```typescript
{
  using guard = adapter.acquireWrite(surface);
  guard.view.draw(...);
}  // [Symbol.dispose] runs here
```

## Composition via capability markers

Outer adapters compose on inner adapters via marker traits. Skia, for
example, builds on Vulkan: it needs the inner adapter's view to expose
a `VkImage`. The Skia adapter constrains the inner type via the
`VulkanWritable` marker:

```rust
impl<Inner> SurfaceAdapter for SkiaAdapter<Inner>
where
    Inner: SurfaceAdapter,
    for<'g> Inner::WriteView<'g>: VulkanWritable,
{
    type WriteView<'g> = SkiaWriteView<'g, Inner>;
    // ... uses inner.view().vk_image() to build a GrVkImageInfo ...
}
```

The customer of `SkiaAdapter` only ever sees `SkSurface`. The inner
view is a private detail of the outer adapter.

`VulkanWritable::vk_image_layout()` is a deliberate escape hatch: Skia's
`GrVkImageInfo` requires the current image layout to build a backend
context. Customers of `SurfaceAdapter` itself never see it — only
adapter authors composing on Vulkan do. The escape hatch is "surfaced
and discussed" rather than "smuggled in" per the engine-model rules
in CLAUDE.md.

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

Polyglot subprocesses (Python, Deno) hold an `OwnedFd`-bound
`StreamlibSurface`. When the subprocess exits cleanly, `Drop` runs
the `streamlib-surface-client::release_surface` request. When the
subprocess crashes mid-write, the kernel closes the inherited fd; the
host's surface-share watchdog (planned, see follow-up issue) observes
`EPOLLHUP` on the per-subprocess Unix socket and decrements the
backing's refcount.

Subprocess Python/Deno code MUST NOT create its own `VkDevice` —
dual `VkDevice` on NVIDIA Linux SIGSEGVs (see
[`docs/learnings/nvidia-dual-vulkan-device-crash.md`](../learnings/nvidia-dual-vulkan-device-crash.md)).
The surface-share IPC hands subprocess code FD-imported memory it
binds onto the host's `VkDevice` via `VkImportMemoryFdInfoKHR` —
that's the only legal Vulkan allocation path on the subprocess side.

## ABI version gate

`STREAMLIB_ADAPTER_ABI_VERSION` (currently `1`) is reported by every
adapter's default `trait_version()` method. The runtime refuses an
adapter whose major doesn't match — `AdapterError::IncompatibleAdapter`
surfaces the mismatch with both versions named.

Adding methods to the trait is a non-breaking minor change. Renaming
or removing a method, or changing an existing signature, is a major.

## Where the code lives

- `libs/streamlib-adapter-abi/` — the contract crate. Trait,
  descriptor, errors, guards, mock, conformance suite, subprocess
  crash harness.
- `libs/streamlib-python/python/streamlib/surface_adapter.py` —
  Python mirror.
- `libs/streamlib-deno/surface_adapter.ts` — Deno mirror.
- `libs/streamlib/src/linux/surface_share/` — host-side backing
  store and the Unix-socket service that hands DMA-BUF fds to
  subprocesses.

In-tree adapter implementations live in their own crates
(`streamlib-adapter-vulkan`, `-opengl`, `-skia`, `-cpu-readback`),
landing in #511–#514.
