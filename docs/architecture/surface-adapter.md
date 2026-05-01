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
impl<Inner> SurfaceAdapter for SkiaAdapter<Inner>
where
    Inner: SurfaceAdapter,
    for<'g> Inner::WriteView<'g>: VulkanImageInfoExt,
{
    type WriteView<'g> = SkiaWriteView<'g, Inner>;
    // inner.view().vk_image_info() fills the entire GrVkImageInfo.
}
```

The customer of `SkiaAdapter` only ever sees `SkSurface`. The inner
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

Polyglot subprocesses (Python, Deno) hold an `OwnedFd`-bound
`StreamlibSurface`. When the subprocess exits cleanly, `Drop` runs
the `streamlib-surface-client::release_surface` request. When the
subprocess crashes mid-write, the kernel closes the per-subprocess
Unix socket; the host's surface-share watchdog observes the
disconnect (kernel-side equivalent of `EPOLLHUP`) and releases every
surface registered under that subprocess's `runtime_id`. The
double-release case is idempotent — a polite `release` followed by a
crash leaves nothing for the watchdog to do.

Subprocess Python/Deno code MUST NOT create its own `VkDevice` —
dual `VkDevice` on NVIDIA Linux SIGSEGVs (see
[`docs/learnings/nvidia-dual-vulkan-device-crash.md`](../learnings/nvidia-dual-vulkan-device-crash.md)).
The surface-share IPC hands subprocess code FD-imported memory it
binds onto the host's `VkDevice` via `VkImportMemoryFdInfoKHR` —
that's the only legal Vulkan allocation path on the subprocess side.

## ABI version gate

`STREAMLIB_ADAPTER_ABI_VERSION` (currently `1`) is the major-version
constant. Adding methods to the trait is non-breaking and does NOT
bump it; renaming or removing a method, or changing an existing
signature or `#[repr(C)]` field, is a major bump.

The trait does not carry a `trait_version()` method — Rust's vtable
layout already enforces in-process compatibility at compile time. A
mismatched `streamlib-adapter-abi` rlib version cannot link into the
runtime in the first place. The constant becomes load-bearing only
at the cdylib boundary, where it'll be checked from a `#[repr(C)]
AdapterDeclaration` shape (mirroring `streamlib-plugin-abi`'s
`PluginDeclaration`) when dynamic adapter loading lands.

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
