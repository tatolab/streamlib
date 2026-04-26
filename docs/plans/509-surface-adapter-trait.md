# Plan — #509: SurfaceAdapter trait + StreamlibSurface descriptor

Foundation ABI for the Surface Adapter Architecture milestone. Lands the
trait, descriptor, scope ABI, polyglot mirrors, conformance suite, and
docs. #510 (host VkImage pool), #511 (Vulkan adapter), #512 (OpenGL),
#513 (Skia), #514 (CPU readback), and #515 (polyglot rewrite) build on
top.

Production-grade by default per CLAUDE.md → "Production-grade by default".

## Crate placement

**New crate `libs/streamlib-adapter-abi/`.** Companion to
`streamlib-plugin-abi` — tiny, dependency-light, the contract that
adapter authors (in-tree and 3rd-party) implement against. Existing
`core::rhi` (compute kernels, GpuContext, RhiPixelBuffer, textures)
stays in `libs/streamlib/src/core/rhi/` untouched.

Why not `streamlib-rhi`: would collide with the existing
`streamlib::core::rhi` runtime-internal module name. `-adapter-abi`
mirrors the existing `-plugin-abi` pattern unambiguously.

Three-crate split for surface concerns:

| Crate | Role |
|---|---|
| `streamlib-adapter-abi` | **New** — surface adapter contract |
| `streamlib-surface-client` | Existing — SCM_RIGHTS Linux wire helper (sits below the trait) |
| `streamlib::core::rhi` | Existing — runtime-internal hardware abstraction |

## Trait shape

### 1. `StreamlibSurface` — `#[repr(C)]` descriptor

```rust
#[repr(C)]
pub struct StreamlibSurface {
    pub id: SurfaceId,            // u64 host-assigned
    pub width: u32,
    pub height: u32,
    pub format: SurfaceFormat,    // #[repr(u32)] enum
    pub usage: SurfaceUsage,      // #[repr(transparent)] bitflags
    transport: SurfaceTransportHandle,  // pub(crate) — DMA-BUF fd + DRM modifier
    sync: SurfaceSyncState,             // pub(crate) — timeline semaphore values, current layout
}

pub type SurfaceId = u64;

#[repr(u32)] pub enum SurfaceFormat { Bgra8 = 0, Rgba8 = 1, Nv12 = 2, /* … */ }

bitflags! {
    #[repr(transparent)] pub struct SurfaceUsage: u32 {
        const RENDER_TARGET = 1 << 0;
        const SAMPLED       = 1 << 1;
        const CPU_READBACK  = 1 << 2;
    }
}
```

Customer-visible fields are `pub`; transport + sync are `pub(crate)`,
exposed only to adapter implementations through `pub(crate)` accessor
methods.

### 2. `SurfaceAdapter` — the trait (GATs, two methods)

```rust
pub trait SurfaceAdapter: Send + Sync {
    type ReadView<'g>  where Self: 'g;
    type WriteView<'g> where Self: 'g;

    fn acquire_read<'g>(&'g self, surface: &StreamlibSurface)
        -> Result<ReadGuard<'g, Self>, AdapterError>;
    fn acquire_write<'g>(&'g self, surface: &StreamlibSurface)
        -> Result<WriteGuard<'g, Self>, AdapterError>;

    fn trait_version(&self) -> u32 { STREAMLIB_ADAPTER_ABI_VERSION }
}

pub const STREAMLIB_ADAPTER_ABI_VERSION: u32 = 1;
```

Two methods (`acquire_read` / `acquire_write`) — type-system enforces
R/W asymmetry the way `RwLock::read()`/`write()` does. `AccessMode`
enum still exists for the IPC wire format and the Python/Deno mirrors
(typestate doesn't translate ergonomically across language boundaries).

GATs — `type ReadView<'g>` — let the view borrow from the adapter for
the guard's lifetime without heap allocation. Stable since Rust 1.65;
workspace MSRV is far past.

### 3. RAII guards

```rust
pub struct ReadGuard<'g, A: SurfaceAdapter + ?Sized> {
    adapter: &'g A,
    surface_id: SurfaceId,
    view: A::ReadView<'g>,
}
impl<'g, A: SurfaceAdapter + ?Sized> ReadGuard<'g, A> {
    pub fn view(&self) -> &A::ReadView<'g> { &self.view }
}
impl<'g, A: SurfaceAdapter + ?Sized> Drop for ReadGuard<'g, A> {
    fn drop(&mut self) { self.adapter.end_read_access(self.surface_id); }
}

// WriteGuard mirrors ReadGuard with `view_mut()` and exclusive-access
// enforcement inside the adapter.
```

Drop signals the release-side timeline semaphore via the adapter's
`end_read_access` / `end_write_access` (sealed trait methods, not
overridable). Customer never types "semaphore."

### 4. Capability marker traits (composition)

```rust
pub trait VulkanWritable {
    fn vk_image(&self) -> ash::vk::Image;
    fn vk_image_layout(&self) -> ash::vk::ImageLayout;
}
pub trait GlWritable { fn gl_texture_id(&self) -> u32; }
pub trait CpuReadable { fn read_bytes(&self) -> &[u8]; }   // forward-compat for #514
pub trait CpuWritable { fn write_bytes(&mut self) -> &mut [u8]; }
```

The Skia adapter can constrain its inner adapter via these:

```rust
impl<Inner> SurfaceAdapter for SkiaAdapter<Inner>
where
    Inner: SurfaceAdapter,
    for<'g> Inner::WriteView<'g>: VulkanWritable,
{
    type WriteView<'g> = SkiaWriteView<'g, Inner>;
    /* SkiaWriteView holds the inner WriteGuard; customer sees only skia::Surface */
}
```

Customer never sees the inner view — it lives inside the outer adapter's
`WriteView` type, used only to construct the framework-idiomatic handle.

`VulkanWritable::vk_image_layout()` is the **deliberate escape hatch**
for the Skia adapter (Skia's `GrVkImageInfo` requires the current layout
to build a backend context). Customers of `SurfaceAdapter` never see it;
only adapter authors composing on Vulkan do. Per CLAUDE.md, this is the
"escape hatch surfaced and discussed" pattern.

## Observability + ABI hygiene

Per CLAUDE.md "Production-grade by default":

- **`tracing::instrument`** on every public method: `acquire_read`,
  `acquire_write`, guard drops. Fields: `surface_id`, `mode`,
  `duration_us`, `adapter_kind`.
- **`AdapterError`** enum with named variants:
  `WriteContended { surface_id, holder }`, `SurfaceNotFound { surface_id }`,
  `IpcDisconnected { reason }`, `SyncTimeout { duration }`,
  `IncompatibleAdapter { trait_version, runtime_version }`,
  `BackingDestroyed { surface_id }`. `thiserror`-derived `Display`,
  no `()` errors, no panic-on-internal-bug.
- **`STREAMLIB_ADAPTER_ABI_VERSION`** u32 constant; `trait_version()`
  default. Runtime can refuse adapters from a future major.
- **Documented stability contract** — what's SemVer-stable, what's an
  extension point.

## Subprocess lifetime — kernel-FD watchdog (option A)

- Host's `SurfaceShareState` (#420) refcounts via `checkout_count`.
- Subprocess holds an `OwnedFd`-bound `StreamlibSurface`; `Drop` runs
  `streamlib-surface-client::release_surface`.
- Subprocess crash mid-write: kernel closes the FD; host's existing
  `epoll(EPOLLHUP)` decrement triggers refcount cleanup. **#509 only
  ships the integration test, not a re-implementation of the watchdog.**
- Heartbeat-style watchdog (catches "alive but wedged") is a separate
  failure class — file as research issue if production observes wedge
  cases. Not preemptive.

## Polyglot mirrors

### Python — `streamlib-python/python/streamlib/surface_adapter.py`

```python
class StreamlibSurface(Protocol):
    id: int
    width: int
    height: int
    format: int
    usage: int

class _StreamlibSurfaceC(ctypes.Structure):
    """Mirror of streamlib_adapter_abi::StreamlibSurface for ctypes drift testing."""
    _fields_ = [
        ("id", ctypes.c_uint64),
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("format", ctypes.c_uint32),
        ("usage", ctypes.c_uint32),
        ("transport", _SurfaceTransportHandleC),
        ("sync", _SurfaceSyncStateC),
    ]

class SurfaceAdapter(Protocol):
    def acquire_read(self, surface: StreamlibSurface) -> ContextManager[ReadView]: ...
    def acquire_write(self, surface: StreamlibSurface) -> ContextManager[WriteView]: ...
```

`with adapter.acquire_write(surface) as view:` — context-manager exit
signals release. `AccessMode` enum mirrored separately for IPC wire.

Test follows `test_vulkan_context.py::test_vulkan_handles_struct_layout_matches_rust_repr_c`
shape: size + per-field offset locked.

### Deno — `streamlib-deno/types/surface_adapter.ts`

```typescript
export interface SurfaceAdapter<RView, WView> {
  acquireRead(surface: StreamlibSurface): { view: RView; [Symbol.dispose](): void };
  acquireWrite(surface: StreamlibSurface): { view: WView; [Symbol.dispose](): void };
}
```

`using guard = adapter.acquireWrite(surface);` — TC39 `using` block
runs `[Symbol.dispose]` at scope end. Layout offsets locked via
`Deno.UnsafePointerView` reads.

### Polyglot conformance

`streamlib_adapter_abi::testing::run_conformance(&adapter)` in Rust;
`streamlib.adapter_abi.testing.run_conformance(adapter)` in Python;
Deno equivalent. Adapter authors in any language can validate against
the same gate.

## Tests

1. **`tests::surface_adapter_conformance`** — generic fixture parameterized
   over `<A: SurfaceAdapter>`. Covers acquire/release ordering,
   double-acquire-write rejection, concurrent-read permission, scope-drop
   sync emission. Run against `MockAdapter`.
2. **`tests::descriptor_repr_c_layout`** — `mem::size_of::<StreamlibSurface>()`
   + per-field `offset_of!` assertions. Twin Python and Deno tests.
3. **`MockAdapter`** — pure-Rust, tracks acquire/release in atomics. Runs
   the conformance suite. Reference for 3rd-party adapter authors.
4. **`SubprocessCrashHarness`** — public test helper. Spawns a
   Python/Deno subprocess, runs a closure, SIGKILLs at a configurable
   point. Reused by #511–#514 for their own crash tests.
5. **Subprocess-crash-mid-write** integration test — uses the harness +
   the existing #420 surface-share service. Asserts host refcount drops
   to zero within 1 s of kill, surface becomes available for new write.

## Files this issue touches

**New (Rust):**
- `libs/streamlib-adapter-abi/Cargo.toml`
- `libs/streamlib-adapter-abi/src/lib.rs`
- `libs/streamlib-adapter-abi/src/surface.rs`
- `libs/streamlib-adapter-abi/src/adapter.rs`
- `libs/streamlib-adapter-abi/src/guard.rs`
- `libs/streamlib-adapter-abi/src/error.rs`
- `libs/streamlib-adapter-abi/src/mock.rs`
- `libs/streamlib-adapter-abi/src/conformance.rs`
- `libs/streamlib-adapter-abi/src/testing.rs`
- `libs/streamlib-adapter-abi/tests/conformance_run.rs`
- `libs/streamlib-adapter-abi/tests/repr_c_layout.rs`
- `libs/streamlib-adapter-abi/tests/subprocess_crash.rs`

**New (polyglot):**
- `libs/streamlib-python/python/streamlib/surface_adapter.py`
- `libs/streamlib-python/python/streamlib/tests/test_surface_adapter.py`
- `libs/streamlib-deno/types/surface_adapter.ts`
- `libs/streamlib-deno/tests/surface_adapter_test.ts`

**New (docs):**
- `docs/architecture/surface-adapter.md`
- `docs/adapter-authoring.md`

**Modified:**
- `Cargo.toml` (workspace member entry)
- `libs/streamlib/Cargo.toml` (depend on `streamlib-adapter-abi`,
  re-export through `core::rhi::surface`)

## Out of scope

- Concrete adapter implementations (#511–#514).
- Host VkImage pool with DRM-modifier export (#510).
- Surface-share service protocol changes (#420 already shipped; trait
  consumes existing API).
- macOS support (milestone description: Linux first).
- Removing `vulkan_context.py` (#515).
- Heartbeat watchdog (file as research issue if observed in production).

## Follow-up issues to file alongside #509

Both independent of #509, both `research`-labeled, target *Stability &
Debuggability Uplift* milestone:

1. **Audit `streamlib-runtime` crate** — what depends on it, can it be
   deleted, who's the consumer? User suspects it's vestige from the
   old CLI/broker design serving a niche web-UI use case.
2. **Crate layout: `streamlib` umbrella + `streamlib-engine`?** —
   research whether the bevy-style umbrella pattern applies (thin
   `streamlib` re-exports `streamlib-engine` + subsystem crates).
   No decision yet; research-gated.
