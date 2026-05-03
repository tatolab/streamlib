# Authoring a new surface adapter

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects code state as of 2026-04-30 (post-#560, #562, #587, #588).
> Verify against current code before generalizing.

This doc is the implementation contract for writing a new
`SurfaceAdapter`. It codifies the patterns the in-tree adapters
(`-vulkan`, `-opengl`, `-cpu-readback`, `-cuda`; `-skia` in flight)
landed on so a new adapter author can land on the right shape
mechanically.

**If you're a customer using an existing adapter**, read
[`surface-adapter.md`](surface-adapter.md) instead — that's the
customer-facing brief.

**If you're writing a new adapter**, read this end-to-end first.
The shape is uniform across every in-tree adapter; deviating from
it is almost always wrong, and the [trip-wires](#trip-wires)
section below lists the cases that look like they justify a
deviation but don't.

## The single-pattern principle

Every surface adapter rides the same shape. The shape is a
deliberate engine-model invariant ([CLAUDE.md → engine
model](../../CLAUDE.md#the-streamlib-engine-model)) — the RHI is
the single gateway to the GPU, and surface adapters are the
single gateway from a host-allocated GPU resource to a customer's
framework-native handle.

The canonical recipe:

1. **The adapter type is generic over `D: VulkanRhiDevice`** from
   `streamlib-consumer-rhi`. The `VulkanRhiDevice` trait, plus the
   companion `DevicePrivilege` / `VulkanTextureLike` /
   `VulkanPixelBufferLike` / `VulkanTimelineSemaphoreLike` traits,
   is everything the adapter needs from the device. The same
   adapter type instantiates against `HostVulkanDevice` host-side
   and `ConsumerVulkanDevice` cdylib-side — same trait surface,
   same acquire/release semantics.

2. **Host setup pre-allocates** whatever per-surface resources the
   adapter needs (an exportable `VkImage` for vulkan/opengl/skia,
   an exportable HOST_VISIBLE staging `VkBuffer` for cpu-readback,
   an OPAQUE_FD-exportable `VkBuffer` for cuda) plus an exportable
   timeline semaphore, and **registers them via surface-share**
   under a UUID. The host RHI does the privileged work
   (modifier discovery, VMA pool selection, cap-handling around
   the swapchain).

3. **Subprocess setup looks up the registration** via surface-share
   and **imports the FDs through `streamlib-consumer-rhi`** —
   `ConsumerVulkanTexture::from_dma_buf_fd`,
   `ConsumerVulkanPixelBuffer::from_dma_buf_fd` (single-plane) /
   `from_dma_buf_fds` (multi-plane) / `from_opaque_fd` (cuda's
   OPAQUE_FD path), and
   `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`
   (timeline semaphores cross processes via OPAQUE_FD only,
   regardless of whether the data resource is DMA-BUF or
   OPAQUE_FD). Then instantiates the **same** adapter type against
   a `ConsumerVulkanDevice`.

4. **Per-acquire is timeline-wait + layout-transition**. Both run
   through traits the carve-out exposes — no privileged ops. If
   the host has work to do per acquire (cpu-readback's
   `vkCmdCopyImageToBuffer`, escalated compute via #550), it's a
   **thin trigger** — IPC publishes a timeline value, the
   subprocess waits on the imported timeline through the carve-
   out. No fresh FD-passing payload per acquire.

5. **Runtime wiring is a single `install_setup_hook` call** at app
   startup (see [Runtime wiring](#runtime-wiring) below). The hook
   captures whatever pre-start state the adapter needs, allocates +
   registers host surfaces, and (for escalate-trigger adapters)
   sets the bridge on `GpuContext`.

That's the full shape. Every in-tree adapter follows it, with the
only meaningful axis of variation being the **handle type** (DMA-BUF
for GPU adapters and cpu-readback's staging buffer; OPAQUE_FD for
cuda's DLPack contract — the wire format carries `handle_type` as
a discriminator).

## Authoring checklist

Mechanical steps — work top-to-bottom.

### 1. Crate layout

Create three crates under `libs/`:

- `streamlib-adapter-<name>/` — the adapter implementation. Runtime
  dep graph: `streamlib-adapter-abi` + `streamlib-consumer-rhi` +
  `streamlib-surface-client` + `vulkanalia`. **Never** depend on
  `streamlib` at runtime — that pulls `HostVulkanDevice` into the
  cdylib's dep graph and breaks the FullAccess capability boundary.
  `streamlib` is allowed as a dev-dependency only.

- `streamlib-adapter-<name>-helpers/` (optional but standard) —
  test-helper bin(s) that need `streamlib`. Held in a separate
  crate so the adapter's runtime dep graph stays `streamlib`-free
  even when its tests bring up a `HostVulkanDevice`. Mirror the
  existing helpers crates' shape; mark `publish = false`.

- The framework-native helper crate, if the adapter needs one
  (e.g. `streamlib-adapter-skia` would have a Skia-binding helper
  crate). Same dep-graph rules apply: anything cdylibs link must
  not pull `streamlib`.

### 2. Module layout in the adapter crate

Use the canonical module split (matches the four shipped adapters):

```
src/
  lib.rs        — crate-root re-exports + module docs
  adapter.rs    — `<Name>SurfaceAdapter<D: VulkanRhiDevice>`,
                  `impl SurfaceAdapter`, `try_begin_*`/`finalize_*`
                  helpers
  context.rs    — `<Name>Context` (high-level customer entry point;
                  optional but conventional)
  state.rs      — `HostSurfaceRegistration`, per-surface `SurfaceState`,
                  `impl SurfaceRegistration` for the registry
  view.rs       — `<Name>ReadView<'g>` / `<Name>WriteView<'g>` and
                  whatever capability-marker impls (`VulkanWritable`,
                  `GlWritable`, `CpuReadable`, …) the adapter exposes
```

If the adapter needs a framework-binding shim that doesn't fit
above (EGL for `-opengl`, raw-handle escape hatches for `-vulkan`,
DLPack for `-cuda`), drop it in its own module — don't shoehorn it
into one of the canonical files.

### 3. Implement the trait

`<Name>SurfaceAdapter<D>` impls `streamlib_adapter_abi::SurfaceAdapter`.
The pattern every in-tree adapter follows:

- Hold a `Registry<SurfaceState<D::Privilege>>` from
  `streamlib-adapter-abi`. Don't roll your own `Mutex<HashMap<SurfaceId, _>>`
  — `Registry` already encodes the read/write contention machine.
- `try_begin_read` / `try_begin_write` snapshot under the registry
  lock and return everything `finalize_*` needs unlocked (timeline
  Arc, current layout, image handle).
- `finalize_*` does the timeline wait + layout transition outside
  the lock, with a rollback path on failure.
- `acquire_*` returns `AdapterError::WriteContended` (with a
  `holder` string identifying who's holding it — `"writer"` from a
  blocked read, the contender's role from a blocked write) when
  `try_begin_*` returns `Ok(None)`; `try_acquire_*` returns
  `Ok(None)` instead.
- `end_read_access` / `end_write_access` (sealed methods called
  from the guard's `Drop`) signal the next timeline value.

`streamlib-adapter-vulkan/src/adapter.rs` is the reference shape.
Read it before you start.

### 4. Implement capability markers

Pick the markers your view exposes from
`streamlib-adapter-abi::adapter`:

| Marker | When to impl | Reference adapter |
|---|---|---|
| `VulkanWritable` (image + layout) | Always, if the view is a `VkImage` | `-vulkan`, `-opengl`'s inner-vulkan view path |
| `VulkanImageInfoExt` (full `VkImageInfo`) | If a Skia-style outer adapter could compose on this | `-vulkan` |
| `GlWritable` (`gl_texture_id`) | OpenGL texture views | `-opengl` |
| `CpuReadable` / `CpuWritable` | **Only** for `-cpu-readback` (architectural — switching to cpu-readback is the contractual signal that the customer opted into a host-side copy) | `-cpu-readback` |

`-cuda` doesn't impl any of the above — it exposes a DLPack
`ManagedTensor` pointer, which is its own framework's idiomatic
shape. New adapters with framework-specific shapes do the same:
expose the native handle on the view directly.

### 5. Tests

Every adapter ships, at minimum:

- `tests/conformance.rs` — calls
  `streamlib_adapter_abi::testing::run_conformance(adapter, factory)`.
  Non-negotiable; the suite exercises blocking and non-blocking
  acquires, RW exclusion, contention errors, and surface
  lifetime.
- `tests/round_trip_*.rs` — host writes, subprocess reads (and
  vice versa for write-capable adapters). Uses the
  `streamlib-adapter-<name>-helpers` bin to spawn a real
  subprocess.
- `tests/subprocess_crash_mid_*.rs` — crashes a subprocess mid-
  acquire and asserts the host watchdog releases the surface.

If the adapter has framework-specific concerns (cpu-readback's
multi-plane stride/offset; cuda's OPAQUE_FD vs DMA-BUF
discrimination), file them as their own focused tests in the
adapter's `tests/` dir.

### 6. Runtime wiring

Adapter authors don't write a runtime hook themselves — application
authors do, when they want to expose the adapter to a subprocess.
The pattern is described in [Runtime wiring](#runtime-wiring) below.
Document the canonical `install_setup_hook` snippet for your
adapter in the crate's top-level `lib.rs` doc-comment so
application authors can copy-paste.

When the adapter's surface is expected to flow downstream to an
**in-process** Rust consumer on the hot path (display, blending
compositor, encoder), the snippet must dual-register the surface
— `gpu.surface_store().register_texture(...)` for cross-process
publishing AND `gpu.register_texture_with_layout(...)` for the
in-process Path 1 fast path. See
[Dual-registration for in-process consumers](adapter-runtime-integration.md#dual-registration-for-in-process-consumers)
in the runtime-integration doc for the rule, the reference
in-tree producer (`LinuxCameraProcessor`), and the cases where
the second call is unnecessary (subprocess-only consumers,
post-stop readback).

### 7. Cross-links

Add the new adapter to:

- [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) — append a
  row to the per-pattern table if the adapter exercises a new
  cell, otherwise just confirm it rides the existing carve-out.
- [`adapter-runtime-integration.md`](adapter-runtime-integration.md)
  — append a row to the recommendation table.
- This doc — add the adapter to the [Reference adapters](#reference-adapters)
  list and update the conformance shape if it surfaced a new
  pattern.

## Crate skeleton

### `Cargo.toml` — adapter crate

```toml
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

[package]
name = "streamlib-adapter-<name>"
description = "<one-line: what the adapter does, what framework, on which platforms>"
version.workspace = true
edition.workspace = true
authors.workspace = true
license-file.workspace = true
repository.workspace = true

[lib]
name = "streamlib_adapter_<name>"
path = "src/lib.rs"

[dependencies]
streamlib-adapter-abi = { path = "../streamlib-adapter-abi" }
thiserror.workspace = true
tracing.workspace = true

[target.'cfg(target_os = "linux")'.dependencies]
streamlib-consumer-rhi = { path = "../streamlib-consumer-rhi" }
streamlib-surface-client = { path = "../streamlib-surface-client" }
vulkanalia.workspace = true
libc.workspace = true

# `streamlib` is dev-only. The runtime crate above does NOT pull
# `streamlib`, so subprocess cdylibs depending on this adapter get
# the consumer-rhi carve-out only and `streamlib` is absent from
# their dep graph (asserted by `cargo tree -p streamlib-{python,deno}-native
# | grep -c "^streamlib v"` returning 0).
[target.'cfg(target_os = "linux")'.dev-dependencies]
streamlib = { path = "../streamlib" }
streamlib-adapter-<name>-helpers = { path = "../streamlib-adapter-<name>-helpers" }
tracing-subscriber.workspace = true

[[test]]
name = "conformance"
path = "tests/conformance.rs"

[lints]
workspace = true
```

### `src/lib.rs` — crate root

```rust
// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! <One-line: what the adapter does.>
//!
//! <Two-paragraph implementation brief: which framework-native handle
//! the customer sees, how the carve-out import path is used, what
//! per-acquire work happens (timeline + layout transition; thin IPC
//! trigger if any).>
//!
//! See [`docs/architecture/surface-adapter.md`](../../docs/architecture/surface-adapter.md)
//! for the architecture brief and
//! [`docs/architecture/adapter-authoring.md`](../../docs/architecture/adapter-authoring.md)
//! for the 3rd-party authoring guide.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod state;
mod view;

pub use adapter::<Name>SurfaceAdapter;
pub use context::<Name>Context;
pub use state::HostSurfaceRegistration;
pub use view::{<Name>ReadView, <Name>WriteView};
```

### `src/adapter.rs` — adapter type skeleton

```rust
// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter,
    SurfaceId, WriteGuard,
};
use streamlib_consumer_rhi::{
    DevicePrivilege, VulkanRhiDevice, VulkanTextureLike, VulkanTimelineSemaphoreLike,
};

use crate::state::{HostSurfaceRegistration, SurfaceState};
use crate::view::{<Name>ReadView, <Name>WriteView};

const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// <Framework>-native [`SurfaceAdapter`] implementation. Generic
/// over the device flavor — instantiate as
/// `<Name>SurfaceAdapter<HostVulkanDevice>` host-side or
/// `<Name>SurfaceAdapter<ConsumerVulkanDevice>` cdylib-side.
pub struct <Name>SurfaceAdapter<D: VulkanRhiDevice> {
    device: Arc<D>,
    surfaces: Registry<SurfaceState<D::Privilege>>,
    acquire_timeout: Duration,
}

impl<D: VulkanRhiDevice> <Name>SurfaceAdapter<D> {
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        // Insert into the registry; return SurfaceAlreadyRegistered
        // on collision. See -vulkan/src/adapter.rs for the exact shape.
        todo!()
    }
}

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for <Name>SurfaceAdapter<D> {
    type ReadView<'g> = <Name>ReadView<'g>;
    type WriteView<'g> = <Name>WriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        // try_begin_read → finalize_read (timeline wait + layout
        // transition) → ReadGuard::new. See -vulkan/src/adapter.rs.
        todo!()
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        todo!()
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        todo!()
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        todo!()
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Decrement read_holders; if last reader, signal the next
        // release timeline value.
        todo!()
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Clear write_held; signal the next release timeline value.
        todo!()
    }
}
```

The skeleton has `todo!()`s deliberately — fill them in by reading
`streamlib-adapter-vulkan/src/adapter.rs` and adapting. The shape
is mechanical: `try_begin_*` snapshots under the registry lock,
`finalize_*` runs unlocked, rollback paths on failure.

## Runtime wiring

Adapter authors expose **what** the adapter needs from the runtime;
application authors **install** it via `install_setup_hook`. Document
the canonical snippet in your crate's top-level doc-comment:

```rust
use std::sync::Arc;
use streamlib::core::runtime::StreamRuntime;
use streamlib_adapter_<name>::<Name>SurfaceAdapter;

let runtime = StreamRuntime::new()?;

runtime.install_setup_hook(move |gpu| {
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let adapter = Arc::new(<Name>SurfaceAdapter::new(Arc::clone(&host_device)));

    // Allocate + register host surface(s) the adapter manages.
    // For DMA-BUF GPU adapters: gpu.acquire_render_target_dma_buf_image
    //   + gpu.surface_store().register_texture(uuid, &texture).
    // For OPAQUE_FD (cuda): HostVulkanPixelBuffer::new_opaque_fd_export
    //   + register with handle_type: "opaque_fd".
    // For cpu-readback: HOST_VISIBLE staging VkBuffer + timeline.

    register_host_surface(&adapter, gpu)?;

    // Escalate-trigger adapters only: wire the bridge so subprocess
    // IPC requests dispatch through the host adapter.
    //   gpu.set_<name>_bridge(Arc::new(BridgeImpl { adapter }));

    Ok(())
});
```

The application calls `install_setup_hook` exactly once per adapter
it wants to expose. The hook fires after `GpuContext::init_for_platform_sync`
has created the live `GpuContext`, before any processor's `setup()`
runs — the window where adapter bridges and pre-allocated host
surfaces have to be in place.

See `examples/polyglot-cpu-readback-blur/src/main.rs` for the
canonical reference (it exercises the bridge path; GPU adapters
omit the `set_*_bridge` step).

The trade-off discussion (explicit registration vs. Cargo-feature
ambient availability) lives in the *Trade-off* section of
[`adapter-runtime-integration.md`](adapter-runtime-integration.md)
— the short version is: **explicit and greppable wins** because
adapter setup is per-runtime and lifetime-controlled, neither of
which a Cargo feature can express.

## Polyglot coverage

If the adapter is supposed to be reachable from Python and Deno
subprocesses (which is the default for any new adapter), follow
[`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md):

- Cdylibs (`streamlib-python-native`, `streamlib-deno-native`) add
  the adapter crate as a runtime dep. The cdylib's dep graph
  must still exclude `streamlib` — `cargo tree -p
  streamlib-python-native | grep -c "^streamlib v"` should return
  `0`. CI enforces this via `cargo xtask check-boundaries` (see
  CLAUDE.md → Vulkan RHI Boundary).
- The Python wrapper at `libs/streamlib-python/python/streamlib/`
  and the Deno wrapper at `libs/streamlib-deno/` mirror the trait
  shape using the language's idiomatic scope binding (`with` for
  Python, `using` for Deno). Schemas at
  `libs/streamlib/schemas/` cover any new escalate ops.
- Polyglot coverage is **both Python AND Deno together** (per
  `polyglot.md`). The only legitimate split is schema-only /
  language-specific by construction; document the reason in the
  PR if you split.

## Cross-process producer composition

When the producer side (subprocess writing into a host-allocated
surface) needs spec-correct cross-process layout coordination
(#633) AND the adapter is **not** the Vulkan adapter, **don't** add
a Vulkan device handle to the adapter. Compose: the customer
dual-registers the same surface with the producer adapter (e.g.
`OpenGlSurfaceAdapter`, `streamlib-adapter-skia` GL backend) AND
the canonical `VulkanSurfaceAdapter`, and the producer's release
path delegates to `VulkanSurfaceAdapter::release_to_foreign` for
the QFOT release barrier + the surface-share `update_image_layout`
publish.

This is the engine-model answer (per CLAUDE.md → "The RHI is the
single gateway"): there is one canonical place per API for that
API's state. The OpenGL adapter does GL only; the Vulkan adapter
does Vulkan only; cross-API composition lives at the SDK / customer
layer. Dawn/Chromium's `SharedImageBacking` + per-API
`*ImageRepresentation` pattern is the same shape; we adopted it
deliberately rather than rederive it under a different name.

### When this applies

A surface adapter needs producer-side cross-process release wiring
when **all three** are true:

1. The adapter writes into a host-allocated DMA-BUF / OPAQUE_FD
   resource. (Read-only adapters never publish layout — they
   consume it.)
2. The adapter's underlying API has no native concept of
   `VkImageLayout` (OpenGL, Skia, future ANGLE/DirectComposition,
   etc.). The Vulkan adapter trivially handles its own QFOT
   release; CUDA's path is buffer-only and structurally has no
   layout to publish (see [CUDA exclusion](#cuda-exclusion)).
3. Cross-process consumers exist that read the surface via Path 2
   `acquire_from_foreign`. Same-process consumers go through Path
   1 and don't need this.

### Implementation pattern

**Host setup hook** — register the surface with surface-share
**including** an exportable `HostVulkanTimelineSemaphore`, even
when the producer adapter doesn't use the timeline itself. The
subprocess's `VulkanSurfaceAdapter::register_host_surface` requires
a timeline; without it the dual-registration call fails:

```rust
let timeline = Arc::new(
    HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)?,
);
store.register_texture(
    SCENARIO_SURFACE_UUID,
    &texture,
    Some(timeline.as_ref()),  // required for dual-registration
    VulkanLayout::GENERAL,    // the producer's post-write layout
)?;
```

The `initial_layout` becomes the Vulkan adapter's `current_layout`
at registration time on the cdylib side, so the QFOT release
barrier issues from the right source layout.

**Subprocess customer** — obtain both contexts from the same
runtime in `setup`, then call the producer adapter's
`release_for_cross_process` after the write block exits:

```python
# Python
self._opengl = OpenGLContext.from_runtime(ctx)
self._vulkan = VulkanContext.from_runtime(ctx)
# ...
with self._opengl.acquire_write(uuid) as view:
    # ... GL writes ...
# acquire_write's __exit__ runs glFinish — writes drained.
self._opengl.release_for_cross_process(uuid, self._vulkan, VkImageLayout.GENERAL)
```

```typescript
// Deno
this.opengl = OpenGLContext.fromRuntime(ctx);
this.vulkan = VulkanContext.fromRuntime(ctx);
// ...
{
  using guard = this.opengl.acquireWrite(uuid);
  // ... GL writes ...
}  // dispose runs glFinish — writes drained.
this.opengl.releaseForCrossProcess(uuid, this.vulkan, VkImageLayout.General);
```

**Producer adapter SDK method** — a thin delegating wrapper:

```python
def release_for_cross_process(self, surface, vulkan_ctx, post_release_layout):
    vulkan_ctx.release_for_cross_process(surface, post_release_layout)
```

That's the entire implementation. The adapter crate (Rust) doesn't
change.

**`VulkanContext.release_for_cross_process` lazily registers** the
surface on its first reference, so the customer doesn't need a
no-op `acquire_*` first to populate the cdylib's surface-id map.
The Vulkan adapter's `register_host_surface` consumes the sync_fd
from the resolved SurfaceHandle and dups its own copies of the
DMA-BUF FDs — the producer adapter's earlier registration is not
disturbed (`SurfaceHandle::sync_fd` is `take`n by the Vulkan
register; the OpenGL register reads only the DMA-BUF FDs and never
touches the sync_fd).

### Why not add a Vulkan device handle to the producer adapter

Two alternatives were considered and rejected:

- **Construction-time** `OpenGlSurfaceAdapter::new(runtime, device)`
  — forces every OpenGL adapter user to wire a Vulkan device, even
  ones that don't need cross-process. Conflates "GL access" with
  "Vulkan release" at the type level.
- **Per-call threaded device**
  `release_for_cross_process<D>(surface, device, …)` — moves the
  device threading to the API surface and still requires the
  adapter to stash a `VkImage` (extra `Arc<dyn VulkanTextureLike>`
  on `HostSurfaceRegistration`). Adapter still has Vulkan
  obligations.

Both options put Vulkan responsibilities on the OpenGL adapter,
which is wrong-shaped per the engine-model rule. Composition is
free; rederivation is expensive.

### CUDA exclusion

The CUDA adapter (`streamlib-adapter-cuda`) does NOT need this
pattern. CUDA's interop is buffer-only by structural constraint:
DLPack requires a flat `void*` device pointer, which forces
`cudaImportExternalMemory(OPAQUE_FD)` →
`cudaExternalMemoryGetMappedBuffer`, which only accepts a
`VkBuffer`. `VkBuffer`s have no `VkImageLayout`, so QFOT-for-layout
is structurally meaningless for CUDA. Cross-process correctness
is provided by the timeline-semaphore alone (the cuda.py
docstring: *"there is no per-acquire IPC — the host's pipeline is
expected to write into the OPAQUE_FD buffer and signal the shared
timeline ambiently."*).

If a future CUDA-on-VkImage path lands (currently unscoped — no
open issue contemplates it), it would inherit this dual-
registration pattern from this section automatically.

### Reference

Implementation: `examples/polyglot-opengl-fragment-shader/`
demonstrates the pattern end-to-end for both Python and Deno
runtimes. The host main.rs allocates and registers the timeline;
the python/deno scenario binaries dual-register and call
`release_for_cross_process` after each GL write block.

Sibling tickets: #644 (OpenGL, this section's source), #645
(Skia GL, inherits the pattern when its host crate ships).

## Conformance & tests

Every adapter passes the conformance suite. The entry point is
`streamlib_adapter_abi::testing::run_conformance(adapter, factory)`
— it takes the adapter and a `Fn(SurfaceId) -> StreamlibSurface`
factory the suite calls per scenario to mint fresh surface
descriptors. Wire it as `tests/conformance.rs`:

```rust
use streamlib_adapter_abi::testing::run_conformance;
use streamlib_adapter_<name>::<Name>SurfaceAdapter;

#[test]
fn conformance() {
    // Bring up the adapter + a per-surface factory closure that
    // registers each id with the adapter and returns a matching
    // StreamlibSurface descriptor. See
    // `streamlib-adapter-vulkan/tests/conformance.rs` for the
    // canonical wiring (host VkDevice setup, render-target
    // allocation, timeline construction).
    let adapter = build_test_adapter();
    run_conformance(&adapter, |id| register_one(&adapter, id));
}
```

If your adapter only needs the simplest CPU-empty surface
descriptor, `streamlib_adapter_abi::testing::empty_surface` is the
ready-made factory.

Round-trip tests live next to it; the `streamlib-adapter-<name>-helpers`
bin is the subprocess spawn target. See
`streamlib-adapter-vulkan/tests/` for a complete example matrix.

## Trip-wires

Cases that look like they justify deviating from the single-pattern
shape but **don't**:

1. **"My adapter needs to allocate something on the subprocess side."**
   No, it doesn't. Escalate the allocation to the host. The
   import-side carve-out (`vkImportMemoryFdInfoKHR`,
   `vkBindBufferMemory`, `vkBindImageMemory`,
   `vkMapMemory`, layout transitions on imported handles, sync
   wait/signal on imported timelines) covers every legitimate
   subprocess Vulkan operation. If the carve-out doesn't cover what
   you need, the answer is to escalate, not to extend the carve-
   out. See [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md).

2. **"My adapter needs its own SPIR-V compute kernel on the
   subprocess side."** No, it doesn't. Use `RegisterComputeKernel`
   + `RunComputeKernel` (#550) to dispatch through the host's
   `VulkanComputeKernel`. The SPIR-V reflection / descriptor-set
   layout / pipeline cache machinery is a single host-side win;
   mirroring it in subprocess code re-introduces every problem
   `core::rhi::ComputeKernelDescriptor` solved once.

3. **"My adapter is a GPU adapter so it can't use surface-share —
   it needs per-acquire FD passing."** No. cpu-readback was
   originally framed this way; the framing was wrong. Pre-register
   resources via surface-share, import them through `consumer-rhi`
   once at registration time. Per-acquire work, when the host has
   any, is a thin trigger that publishes a timeline value — not a
   fresh FD-passing payload.

4. **"My adapter wants per-acquire host work + GPU adapter
   semantics."** Fine — that's what the cpu-readback bridge
   pattern is for. Add a `set_<name>_bridge` setter on `GpuContext`
   (mirroring `set_cpu_readback_bridge`), wire the bridge in
   `install_setup_hook`, dispatch a thin IPC trigger per acquire.
   The subprocess waits on the imported timeline through the
   carve-out as usual.

5. **"My adapter's framework needs a different external-handle
   type than DMA-BUF."** This is real (cuda needs OPAQUE_FD per
   the DLPack contract). The plumbing exists: `RhiExternalHandle`
   has `DmaBuf` and `OpaqueFd` variants, the surface-share wire
   format carries `handle_type` as a discriminator,
   `ConsumerVulkanDevice::import_opaque_fd_memory` exists. Pick
   the variant your framework requires; don't invent a third seam.

6. **"My adapter is hot-path — IPC roundtrips will kill perf."**
   If the adapter rides surface-share-only (no per-acquire IPC),
   acquire is a local timeline wait + layout transition. Sub-
   millisecond. If it rides escalate-trigger and the trigger
   shows up in profiles at frame rate, the answer is to **batch
   triggers** (one IPC covering N frames) — not to invent a
   shared-memory ring or third seam. File a follow-up before
   building one.

7. **"My adapter is read-only (or write-only)."** Implement both
   `acquire_read` and `acquire_write`; have the unsupported
   direction return `AdapterError::BackendRejected` with a
   `reason` that explains the limit. The trait shape is uniform;
   opt-out is per-call, not per-trait. (If you find a real adapter
   class with this shape, file a follow-up to add a dedicated
   error variant.)

If your situation genuinely doesn't fit any of the above and you
believe the single-pattern principle is wrong for it, **stop and
surface the disagreement before building a parallel shape.** That
conversation belongs in an issue, not in code.

## Hypothetical walkthrough — Metal on macOS via MoltenVK

Sanity check: applying the checklist to an adapter not yet shipped.
The exercise is to confirm the checklist would produce the right
shape mechanically.

**Goal**: `streamlib-adapter-metal` exposes a host-allocated
`VkImage` (allocated through the macOS-flavor `HostVulkanDevice`
running on MoltenVK) as an `MTLTexture` for customers writing
Metal-native code.

Walking the checklist:

1. **Crate layout** — three crates: `streamlib-adapter-metal`,
   `streamlib-adapter-metal-helpers` (test-only), and (likely) a
   `streamlib-adapter-metal-mtltexture-bridge` crate that holds
   the unsafe Objective-C bridging code. Same dep-graph rules:
   the runtime adapter crate depends on `streamlib-adapter-abi`
   + `streamlib-consumer-rhi` + `streamlib-surface-client` +
   `vulkanalia`, but **not** `streamlib`.

2. **Module layout** — same five files (`lib.rs`, `adapter.rs`,
   `context.rs`, `state.rs`, `view.rs`) plus a sixth `mtl.rs` for
   the MoltenVK ↔ Metal handle conversion (analogous to
   `-opengl/src/egl.rs`).

3. **The trait impl is generic over `D: VulkanRhiDevice`.**
   `MetalSurfaceAdapter<HostVulkanDevice>` runs in-process Rust;
   `MetalSurfaceAdapter<ConsumerVulkanDevice>` runs cdylib-side.
   Per-acquire is a timeline wait + a layout transition into
   `VK_IMAGE_LAYOUT_GENERAL` plus a MoltenVK-specific call to
   surface the underlying `id<MTLTexture>` — but the `MTLTexture`
   handle is read-only metadata on the imported `VkImage`, not a
   privileged op.

4. **Capability marker** — a new `MetalWritable` marker exposing
   `mtl_texture(&self) -> *const MTLTexture` (or analogous Rust-
   side handle type). Lives in `streamlib-adapter-abi`. Existing
   markers (`VulkanWritable`, `GlWritable`) stay untouched — the
   adapter can also impl `VulkanWritable` if customers want to
   issue MoltenVK Vulkan calls against the same image.

5. **Tests** — conformance suite + macOS-specific round-trips.
   Per [`.claude/workflows/macos.md`](../../.claude/workflows/macos.md),
   cross-compile verification on Linux is required; the
   walkthrough lands the cross-compile + native-macOS CI lane in
   the same milestone.

6. **Runtime wiring** — `runtime.install_setup_hook` allocates a
   host `VkImage` via `gpu.acquire_render_target_image` (the
   macOS analog of `_dma_buf_image`), registers via surface-share,
   no bridge needed because there's no per-acquire host work.

7. **Polyglot** — both Python and Deno subprocesses get the
   `MetalContext` + scope-bound acquire shape. Schemas don't
   change (no new escalate op).

The checklist produced the right shape: the adapter is a thin
Metal-binding layer on top of the existing single-pattern
surface-share shape. The MoltenVK / Metal handle conversion is
genuinely framework-specific (lives in `mtl.rs` / the bridge
crate); everything else is mechanical.

Trip-wires that **didn't** fire: no subprocess-side allocation, no
subprocess-side compute kernel, no per-acquire FD passing, no
custom synchronization. If any of those had been needed, that
would have been the signal to stop and surface the disagreement —
but they weren't.

## Reference adapters

Read these, in this order, when authoring:

| Adapter | What it shows |
|---|---|
| [`streamlib-adapter-vulkan`](../../libs/streamlib-adapter-vulkan/) | Canonical shape. Start here. |
| [`streamlib-adapter-opengl`](../../libs/streamlib-adapter-opengl/) | Composing on Vulkan via EGL DMA-BUF import; framework-binding shim in its own module. |
| [`streamlib-adapter-cpu-readback`](../../libs/streamlib-adapter-cpu-readback/) | Bridge / escalate-trigger pattern. Multi-plane staging buffers. |
| [`streamlib-adapter-cuda`](../../libs/streamlib-adapter-cuda/) | OPAQUE_FD handle type. DLPack-flavored framework-native handle (no `VulkanWritable`-style marker). |

`-skia` (#513) lands on the same shape; check its source once it
ships.

## Related

- [`surface-adapter.md`](surface-adapter.md) — customer-facing brief.
- [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) —
  per-pattern bucketing of host-only vs. carve-out vs. escalate.
- [`adapter-runtime-integration.md`](adapter-runtime-integration.md)
  — *how* a subprocess obtains an adapter context end-to-end;
  `install_setup_hook` mechanics; explicit-vs-Cargo-feature
  trade-off.
- [`compute-kernel.md`](compute-kernel.md) — host's
  `VulkanComputeKernel`, the dispatch primitive any adapter that
  needs compute reaches through (post-#550 via escalate IPC from
  subprocess).
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md)
  — polyglot rules including the import-side carve-out.
- [`.claude/workflows/adapter.md`](../../.claude/workflows/adapter.md)
  — auto-loaded by `/amos:next` for `adapter`-labeled work; points
  back at this doc.
