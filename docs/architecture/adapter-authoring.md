# Authoring a new surface adapter

> Current shipped state only, per
> [`.claude/rules/docs-policy.md`](../../.claude/rules/docs-policy.md).

This doc is the implementation contract for writing a new
`SurfaceAdapter`. It codifies the patterns the in-tree adapters
(`-vulkan`, `-opengl`, `-cpu-readback`, `-cuda`, `-skia`) landed on
so a new adapter author can land on the right shape mechanically.

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
deliberate engine-model invariant
([`.claude/rules/rhi.md`](../../.claude/rules/rhi.md)) — the RHI is
the single gateway to the GPU, and surface adapters are the
single gateway from a host-allocated GPU resource to a customer's
framework-native handle.

The canonical recipe:

1. **The adapter type is generic over `D: VulkanRhiDevice`** from
   `streamlib-consumer-rhi`. The `VulkanRhiDevice` trait, plus the
   companion `DevicePrivilege` / `VulkanTextureLike` /
   `VulkanRhiBuffer` / `VulkanTimelineSemaphoreLike` traits,
   is everything the adapter needs from the device. The same
   adapter type instantiates against `HostVulkanDevice` host-side
   and `ConsumerVulkanDevice` cdylib-side — same trait surface,
   same acquire/release semantics.

2. **Host setup pre-allocates** whatever per-surface resources the
   adapter needs (an exportable `VkImage` for vulkan/opengl/skia,
   an exportable HOST_VISIBLE staging `VkBuffer` for cpu-readback,
   an OPAQUE_FD-exportable `VkBuffer` for cuda) plus **two
   exportable timeline semaphores** (`produce_done` + `consume_done`,
   one per direction of the producer ↔ consumer edge — see
   [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
   for the single-writer-per-edge contract), and **registers them
   via surface-share** under a UUID. The host RHI does the
   privileged work (modifier discovery, VMA pool selection,
   cap-handling around the swapchain).

3. **Subprocess setup looks up the registration** via surface-share
   and **imports the FDs through `streamlib-consumer-rhi`** —
   `ConsumerVulkanTexture::from_dma_buf_fd`,
   `ConsumerVulkanBuffer::from_dma_buf_fd` (single-plane) /
   `from_dma_buf_fds` (multi-plane) / `from_opaque_fd` (cuda's
   OPAQUE_FD path), and a pair of
   `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`
   (one per edge; timeline semaphores cross processes via OPAQUE_FD
   only, regardless of whether the data resource is DMA-BUF or
   OPAQUE_FD). Then instantiates the **same** adapter type against
   a `ConsumerVulkanDevice`.

4. **Per-acquire is `produce_done`-wait + layout-transition**. Both
   run through traits the carve-out exposes — no privileged ops. If
   the host has work to do per acquire (cpu-readback's
   `vkCmdCopyImageToBuffer`, escalated compute / graphics /
   ray-tracing dispatch), it's a **thin trigger** — IPC publishes
   the next `produce_done` value, the subprocess waits on the
   imported `produce_done` through the carve-out, then signals
   `consume_done` from `end_read_access` once the read completes.
   No fresh FD-passing payload per acquire.

5. **Runtime wiring is a single `install_setup_hook` call** at app
   startup (see [Runtime wiring](#runtime-wiring) below). The hook
   captures whatever pre-start state the adapter needs and
   allocates + registers host surfaces. Nothing is installed on
   `GpuContext` — every escalate op it answers is always present.

That's the full shape. Every in-tree adapter follows it, with the
only meaningful axis of variation being the **handle type** (DMA-BUF
for GPU adapters and cpu-readback's staging buffer; OPAQUE_FD for
cuda's DLPack contract — the wire format carries `handle_type` as
a discriminator).

## Authoring checklist

Mechanical steps — work top-to-bottom.

### 1. Crate layout

Create one crate under `adapters/`:

- `streamlib-adapter-<name>/` — the adapter implementation. Runtime
  dep graph: `streamlib-surface-adapter` + `streamlib-consumer-rhi` +
  `streamlib-surface-client` + `vulkanalia`. **Never** depend on
  `streamlib` at runtime — that pulls `HostVulkanDevice` into the
  cdylib's dep graph and breaks the FullAccess capability boundary.
  `streamlib` is allowed as a dev-dependency only.

- The subprocess test helper is an in-crate `[[bin]]` at
  `tests/bin/<name>_adapter_subprocess_helper.rs`, not a separate
  crate. It imports through `streamlib-consumer-rhi` only, so the
  crate's runtime dep graph stays `streamlib`-free even though the
  tests that spawn it bring up a `HostVulkanDevice` from
  `[dev-dependencies]`. Cargo's `[[bin]]` targets don't see
  dev-deps, so anything the helper itself needs goes under regular
  `[dependencies]` — see `streamlib-adapter-skia/Cargo.toml`.

- A framework binding comes from a published crate (`skia-safe` for
  `-skia`) plus an in-crate glue module, not a companion crate.
  Same dep-graph rule applies: nothing the adapter links at runtime
  may pull `streamlib`.

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

`<Name>SurfaceAdapter<D>` impls `streamlib_surface_adapter::SurfaceAdapter`.
The pattern every in-tree adapter follows:

- Hold a `Registry<SurfaceState<D::Privilege>>` from
  `streamlib-surface-adapter`. Don't roll your own `Mutex<HashMap<SurfaceId, _>>`
  — `Registry` already encodes the read/write contention machine.
- `try_begin_read` / `try_begin_write` snapshot under the registry
  lock and return everything `finalize_*` needs unlocked (the
  relevant timeline Arc — `produce_done` for reads, `consume_done`
  for writers waiting on prior consumers — current layout, image
  handle).
- `finalize_*` does the timeline wait + layout transition outside
  the lock, with a rollback path on failure.
- `acquire_*` returns `AdapterError::WriteContended` (with a
  `holder` string identifying who's holding it — `"writer"` from a
  blocked read, the contender's role from a blocked write) when
  `try_begin_*` returns `Ok(None)`; `try_acquire_*` returns
  `Ok(None)` instead.
- `end_read_access` (sealed method called from the guard's `Drop`)
  signals the next `consume_done` value; `end_write_access` signals
  the next `produce_done` value. See
  [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
  for the single-writer-per-edge contract.

`streamlib-adapter-vulkan/src/adapter.rs` is the reference shape.
Read it before you start.

### 4. Implement capability markers

Pick the markers your view exposes from
`streamlib-surface-adapter::adapter`:

| Marker | When to impl | Reference adapter |
|---|---|---|
| `VulkanWritable` (image + layout) | Always, if the view is a `VkImage` | `-vulkan` |
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
  `streamlib_surface_adapter::testing::run_conformance(adapter, factory)`.
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
streamlib-surface-adapter = { path = "../streamlib-surface-adapter" }
thiserror.workspace = true
tracing.workspace = true

[target.'cfg(target_os = "linux")'.dependencies]
streamlib-consumer-rhi = { path = "../../runtime/streamlib-consumer-rhi", version = "0.17.0" }
streamlib-surface-client = { path = "../../runtime/streamlib-surface-client", version = "0.17.0" }
vulkanalia.workspace = true
libc.workspace = true

# `streamlib` is dev-only. The runtime crate above does NOT pull
# `streamlib`, so subprocess code depending on this adapter gets the
# consumer-rhi carve-out only and `streamlib` is absent from its dep
# graph (enforced by `cargo xtask check-boundaries`).
[target.'cfg(target_os = "linux")'.dev-dependencies]
streamlib-engine = { path = "../../runtime/streamlib-engine" }
streamlib = { path = "../../sdk/streamlib-sdk" }
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

use streamlib_surface_adapter::{
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
        // consume_done value.
        todo!()
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Clear write_held; signal the next produce_done value.
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
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib_adapter_<name>::<Name>SurfaceAdapter;

let runtime = Runner::new()?;

runtime.install_setup_hook(move |gpu| {
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let adapter = Arc::new(<Name>SurfaceAdapter::new(Arc::clone(&host_device)));

    // Allocate + register host surface(s) the adapter manages, plus
    // the two per-edge timelines (produce_done + consume_done) per
    // adapter-timeline-single-writer.md.
    // For DMA-BUF GPU adapters: gpu.acquire_render_target_dma_buf_image
    //   + gpu.surface_store().register_texture(uuid, &texture,
    //     Some(produce_done.as_ref()), Some(consume_done.as_ref()),
    //     current_image_layout).
    // For OPAQUE_FD (cuda): HostVulkanBuffer::new_opaque_fd_export
    //   + register with handle_type: "opaque_fd" plus the two
    //     OPAQUE_FD-exportable timelines.
    // For cpu-readback: HOST_VISIBLE staging VkBuffer + the two
    //   timelines via register_pixel_buffer_with_timeline.

    register_host_surface(&adapter, gpu)?;

    Ok(())
});
```

The application calls `install_setup_hook` exactly once per adapter
it wants to expose. The hook fires after `GpuContext::init_for_platform_sync`
has created the live `GpuContext`, before any processor's `setup()`
runs — the window where pre-allocated host surfaces have to be in
place.

An adapter serves consumers in this process. A subprocess reaching a
surface goes through the escalate ops instead, which `GpuContext`
answers directly — there is nothing for an adapter to register on
its behalf.

The trade-off discussion (explicit registration vs. Cargo-feature
ambient availability) lives in the *Trade-off* section of
[`adapter-runtime-integration.md`](adapter-runtime-integration.md)
— the short version is: **explicit and greppable wins** because
adapter setup is per-runtime and lifetime-controlled, neither of
which a Cargo feature can express.

> A "Polyglot coverage" section was removed here: every artifact it named —
> the `streamlib-python-native` / `streamlib-deno-native` cdylibs, the
> `sdk/streamlib-python/` adapter mirror, `packages/escalate/schemas/`, and the
> Deno half of "both Python AND Deno together" — has been deleted. Adapters are
> statically linked into the wheel, and a helper process imports that same wheel
> rather than a separate cdylib.

## Cross-process producer composition

When the producer side (subprocess writing into a host-allocated
surface) needs spec-correct cross-process layout coordination
AND the adapter is **not** the Vulkan adapter, **don't** add
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
**including** the two exportable `HostVulkanTimelineSemaphore`s
(`produce_done` + `consume_done`), even when the producer adapter
doesn't drive them itself. The subprocess's
`VulkanSurfaceAdapter::register_host_surface` requires both
timelines; without them the dual-registration call fails. See
[`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
for the single-writer-per-edge contract.

```rust
let produce_done = Arc::new(
    HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)?,
);
let consume_done = Arc::new(
    HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)?,
);
store.register_texture(
    SCENARIO_SURFACE_UUID,
    &texture,
    Some(produce_done.as_ref()),
    Some(consume_done.as_ref()),
    VulkanLayout::GENERAL,    // the producer's post-write layout
)?;
```

The `initial_layout` becomes the Vulkan adapter's `current_layout`
at registration time on the cdylib side, so the QFOT release
barrier issues from the right source layout.

> A "Subprocess customer / producer adapter SDK method" passage was removed
> here: the Python and Deno `release_for_cross_process` snippets, the thin
> delegating SDK wrapper, and the `VulkanContext` lazy-registration note —
> no crate or stub defines `release_for_cross_process`, the wheel exposes no
> `OpenGLContext` / `VulkanContext`, the Deno SDK is deleted, and there is
> no `SurfaceHandle` type.

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

### CUDA two-flavor split

The CUDA adapter (`streamlib-adapter-cuda`) carries two resource
flavors with different QFOT requirements.

**Flat-tensor DLPack path (`VkBuffer`)** — does NOT need this
pattern. The interop is buffer-only by structural constraint:
DLPack requires a flat `void*` device pointer, which forces
`cudaImportExternalMemory(OPAQUE_FD)` →
`cudaExternalMemoryGetMappedBuffer`, which only accepts a
`VkBuffer`. `VkBuffer`s have no `VkImageLayout`, so QFOT-for-layout
is structurally meaningless. Cross-process correctness for this
path is provided by the `produce_done` + `consume_done` timeline
pair alone (the host pipeline writes into the OPAQUE_FD buffer and
signals `produce_done` ambiently; the cdylib waits on `produce_done`
before reading and signals `consume_done` in `end_read_access`).

**Tiled-image path (`VkImage`)** — inherits this dual-registration
pattern. CUDA's
`cudaExternalMemoryGetMappedMipmappedArray` consumes an OPAQUE_FD
`VkImage` (`HostVulkanTexture::new_opaque_fd_export` on the host,
`ConsumerVulkanTexture::from_opaque_fd` on the cdylib) and produces
a mipmapped-array handle backing `cudaSurfaceObject_t` /
`cudaTextureObject_t` for hardware-bilinear sampling and surface
writes. `VkImage` *does* have a `VkImageLayout`, so the same
cross-process layout coordination story applies as for the
OpenGL / Vulkan adapters: the cdylib's consumer-side acquire
either chains `VkExternalMemoryAcquireUnmodifiedEXT` on a QFOT
acquire (Mesa drivers exposing the extension) or bridges
`UNDEFINED → target` (NVIDIA — empirically content-preserving via
the DMA-BUF / OPAQUE_FD kernel cache). The dual-registration
pattern (cross-process publish + same-process Path-1 entry when an
in-process hot-path consumer also reads the surface) applies
unchanged.

> > A "Reference" section was removed here: `examples/polyglot-opengl-
> fragment-shader/runner/`, its Python and Deno scenario binaries, and their
> `release_for_cross_process` calls — the example directory is gone, the
> Deno SDK is deleted, and no crate defines `release_for_cross_process`.

## Conformance & tests

Every adapter passes the conformance suite. The entry point is
`streamlib_surface_adapter::testing::run_conformance(adapter, factory)`
— it takes the adapter and a `Fn(SurfaceId) -> StreamlibSurface`
factory the suite calls per scenario to mint fresh surface
descriptors. Wire it as `tests/conformance.rs`:

```rust
use streamlib_surface_adapter::testing::run_conformance;
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
descriptor, `streamlib_surface_adapter::testing::empty_surface` is the
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
   subprocess side."** No, it doesn't. Use the
   `register_compute_kernel` + `run_compute_kernel` escalate ops
   to dispatch through the host's `VulkanComputeKernel`. The
   SPIR-V reflection / descriptor-set layout / pipeline cache
   machinery is a single host-side win; mirroring it in
   subprocess code re-introduces every problem
   `core::rhi::ComputeKernelDescriptor` solved once.

3. **"My adapter is a GPU adapter so it can't use surface-share —
   it needs per-acquire FD passing."** No. cpu-readback was
   originally framed this way; the framing was wrong. Pre-register
   resources via surface-share, import them through `consumer-rhi`
   once at registration time. Per-acquire work, when the host has
   any, is a thin trigger that publishes a timeline value — not a
   fresh FD-passing payload.

4. **"My adapter wants per-acquire host work on a subprocess's
   behalf."** That is not an adapter's job. Per-acquire host work
   for a subprocess is an escalate op answered by `GpuContext`,
   which owns the staging and signals the timeline the subprocess
   waits on — see `run_cpu_readback_copy`. Adding an installable
   bridge would reintroduce the application-glue step the engine
   deleted.

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

> A "Hypothetical walkthrough — Metal on macOS via MoltenVK" section was
> removed here: 70 lines applying the checklist to `streamlib-adapter-
> metal`, an adapter its own text calls "not yet shipped". Apple support is
> post-MVP and undesigned, and architecture docs carry no proposed work.

## Reference adapters

Read these, in this order, when authoring:

| Adapter | What it shows |
|---|---|
| [`streamlib-adapter-vulkan`](../../adapters/streamlib-adapter-vulkan/) | Canonical shape. Start here. |
| [`streamlib-adapter-opengl`](../../adapters/streamlib-adapter-opengl/) | Composing on Vulkan via EGL DMA-BUF import; framework-binding shim in its own module. |
| [`streamlib-adapter-cpu-readback`](../../adapters/streamlib-adapter-cpu-readback/) | Bridge / escalate-trigger pattern. Multi-plane staging buffers. |
| [`streamlib-adapter-cuda`](../../adapters/streamlib-adapter-cuda/) | OPAQUE_FD handle type. DLPack-flavored framework-native handle (no `VulkanWritable`-style marker). |
| [`streamlib-adapter-skia`](../../adapters/streamlib-adapter-skia/) | Composes on the Vulkan adapter (Skia Vulkan backend); also offers a GL backend that composes on the OpenGL adapter. |

## Related

- [`surface-adapter.md`](surface-adapter.md) — customer-facing brief.
- [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) —
  per-pattern bucketing of host-only vs. carve-out vs. escalate.
- [`adapter-runtime-integration.md`](adapter-runtime-integration.md)
  — *how* a subprocess obtains an adapter context end-to-end;
  `install_setup_hook` mechanics; explicit-vs-Cargo-feature
  trade-off.
- [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
  — single-writer-per-edge contract for the `produce_done` +
  `consume_done` timeline pair every subprocess-wired adapter
  registers with surface-share.
- [`compute-kernel.md`](compute-kernel.md) — host's
  `VulkanComputeKernel`, the dispatch primitive any adapter that
  needs compute reaches through (via escalate IPC from
  subprocess).
- [`.claude/rules/rhi.md`](../../.claude/rules/rhi.md)
  — the RHI + import-side carve-out rule adapter work rides.
