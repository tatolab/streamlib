# Plugin ABI

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Verify against current code before generalizing.

## What this is

The streamlib plugin ABI is the `#[repr(C)]` wire-protocol header that
lets a host binary and a `dlopen`-loaded Rust cdylib communicate
without sharing any Rust types beyond primitives and `extern "C" fn`
pointers. Loosely analogous to Unreal's `IModuleInterface` or VST3's
audio-plugin spec.

The deployment model the ABI enables: computer A builds the host
binary, computer B builds packages via CI, computer C ships their own
packages — all using different rustc minor versions and different
Cargo dep resolutions, all interoperating, as long as they target the
same triple and pin the same `STREAMLIB_ABI_VERSION` +
`streamlib-consumer-rhi` version. No commit-level coupling, no shared
`Cargo.lock`.

To the best of our current knowledge this is the canonical engine
substrate every cross-repo streamlib package rides; if a plugin
crosses the boundary through anything other than what this doc
describes, that path is either an in-tree-only fast path documented
explicitly or a defect.

## Crate topology

Four crates participate in the ABI surface. The split is deliberate
and load-bearing — collapsing any two re-introduces a capability or
coupling leak.

| Crate | Role | Linked by |
|---|---|---|
| `streamlib-plugin-abi` | Pure `#[repr(C)]` wire shapes + ABI version constants. No methods, no engine internals. Layout regression tests pin every struct's byte layout. | Host engine AND every cdylib. |
| `streamlib-consumer-rhi` | Consumer-side carve-out of the Vulkan RHI. Holds `ConsumerVulkanDevice`, `ConsumerVulkanTexture`, `ConsumerVulkanBuffer`, `ConsumerVulkanTimelineSemaphore`, `VulkanLayout`, `TextureFormat`, `TextureUsages`, `PixelFormat`, and the `VulkanRhiDevice` / `DevicePrivilege` / `VulkanTextureLike` / `VulkanTimelineSemaphoreLike` trait machinery. | Host engine AND every cdylib that touches GPU. |
| `streamlib-engine` (host-side) | Privileged engine internals: `HostVulkanDevice`, VMA pools, queue mutex, modifier probe, kernel construction, swapchain. Plus the host implementations of every vtable callback (in `core/plugin/host_services.rs`). | Host process only. Cdylibs CANNOT Cargo-dep this. |
| `streamlib-sdk` | Thin re-export façade. Cdylibs Cargo-dep `streamlib = "..."` and get the PluginAbiObject types, the `processor` macro, the lifecycle traits — without reaching `HostVulkanDevice` etc. | Host AND cdylibs (re-exports the safe surface from `streamlib-engine`). |

The capability boundary is enforced by the type system for **subprocess
native cdylibs** (`streamlib-python-native`, `streamlib-deno-native`):
their `cargo tree` excludes `streamlib-engine`, so they physically
cannot reach `HostVulkanDevice` or any other privileged primitive.
`cargo tree -p streamlib-python-native | grep -c "^streamlib-engine v"`
returns 0 — that's the lock.

**Facade plugins are a distinct cdylib flavor** and do NOT exclude the
engine. A package that Cargo-deps `streamlib` (the `streamlib-sdk`
façade — e.g. `packages/camera`, `packages/h264`, the
`streamlib-cross-rustc-fixture`) pulls `streamlib-engine` **into the
cdylib** as a statically-linked copy. That embedded engine copy is
precisely what makes the `GpuContextFullAccess` raw-`Arc` slots
(`host_vulkan_device_arc`, `host_vulkan_texture_arc`, the
timeline-semaphore create/set/wait/get) callable from the plugin — and
what the build-fingerprint handshake (below) guards, because a
separately-built copy of the engine can lay those non-`#[repr(C)]`
transit types out differently. The capability *split* still holds for
these plugins (FullAccess is reachable only inside `escalate(|full|
…)`); what differs is that the engine is present in the address space,
not absent.

## Host-side and in-repo packages — workspace members, not registry plugins

Most `packages/*` crates are standalone, registry-distributed plugins. Three
are workspace-member exceptions carved out of that rule — none is shipped
through the registry, so each path-deps the zone crates and carries
`publish = false` (which also keeps it out of the release closure). They fall
into two classes.

**Host-side (`packages/api-server`).** A **host-side package** is statically
linked into the host binary and registered in-process on `PROCESSOR_REGISTRY`,
rather than built as a `cdylib` and loaded through the plugin ABI. It is a
host, not a plugin: it ships no `cdylib`, and nothing it does crosses the ABI.

The api-server is the canonical host-side package. It drives
`RuntimeOperations`, the processor registry, pubsub, and the graph API —
control-plane surfaces a plugin cannot reach, because the ABI exposes the
processor-authoring surface, not runtime control. `streamlib-runtime`
Cargo-deps it as an `rlib` and calls
`PROCESSOR_REGISTRY.register::<ApiServerProcessor::Processor>()` at boot;
its own `Cargo.toml` carries `crate-type = ["rlib"]` and `publish = false`.

The pubsub bridge makes the split concrete. Only `PUBSUB.publish` is
ABI-bridged — a plugin's `publish` forwards to the host's `pubsub_publish`
callback (`HostServices::pubsub_publish`). `PUBSUB.subscribe` is
intentionally **not** bridged: a loaded plugin's own `PUBSUB` static is
never `init()`ed, so `subscribe` buffers into `pending_subscriptions`
forever. The api-server's WebSocket handler subscribes to every topic — in
process on the host's initialized bus that stream is live; as a plugin it
would be dead. That is why the control plane is host-side by construction.

**In-repo test infrastructure (`packages/test-fixtures`,
`packages/test-fixtures-abi-mismatch`).** These two are workspace members for a
different reason: they are in-repo, path-dep test infrastructure, never
distributed. They *are* built as plugin cdylibs — but only in place, for the
engine's own dlopen tests, never shipped. `packages/test-fixtures` supplies the
attribute-macro fixtures (e.g. `TestConfiguredProcessor`) that the dlopen
integration suite builds (`cargo build -p streamlib-test-fixtures --features
plugin`) and loads to exercise the full ABI roundtrip.
`packages/test-fixtures-abi-mismatch` is a hand-crafted `STREAMLIB_PLUGIN` with
a deliberately tampered `abi_version` — the negative fixture the load-path test
loads to prove `validate_plugin_declaration` rejects a mismatched ABI. They are
durable until the facade flavor's own test surface retires.

## The vtable catalog

Every plugin ABI call dispatches through a `#[repr(C)]` vtable whose
layout is pinned by a regression test in `streamlib-plugin-abi`. To
the best of our current knowledge the in-tree set as of this doc is:

### Core lifecycle + services

- **`HostServices`** — wire struct the host fills out and passes to
  the cdylib's `install_host_services` at load time. Carries the
  process-wide service callbacks (tracing emit, PUBSUB publish, schema
  register/lookup, iceoryx2-log emit, processor register) plus
  references to every other vtable on this list. The cdylib reads
  `abi_layout_version` first and only touches fields advertised by its
  layout version.
- **`ProcessorVTable`** — every `extern "C" fn` slot the host invokes
  on a registered processor: constructor, `setup` / `start` /
  `process` / `stop` / `teardown`, lifecycle callbacks (`on_pause`,
  `on_resume`), execution-config + config-json IO, iceoryx2-resource
  binding, plus async-lifecycle wrappers.
- **`PluginDeclaration`** — the static `STREAMLIB_PLUGIN` symbol
  every cdylib exports; carries the `abi_version` constant, the
  `install_host_services` callback, and the package descriptor that
  the host registers schemas + processors from.

### Runtime + audio

- **`RuntimeContextVTable`** — the cdylib's `RuntimeContext` shim
  dispatches `runtime_id`, `tokio_handle`, etc. through this vtable.
- **`AudioClockVTable`** — audio-tick subscription + monotonic-clock
  reads.
- **`RuntimeOpsVTable`** — submit-with-completion runtime mutation
  ops (`add_processor`, `remove_processor`, `connect`, `disconnect`,
  `to_json`) plus `clone_handle` / `drop_handle` for the cdylib's
  owning-Arc shim.

### GPU capability tiers

- **`GpuContextLimitedAccessVTable`** — every `pub fn` reachable from
  cdylib code without escalation: resource acquire (pixel buffer,
  texture, staging buffer), surface-store integration, command-queue
  submit, texture-ring slot rotation + copy, the native-DMA-BUF-FD
  accessor, video-source timeline-semaphore wiring. Every Arc-holding
  return type carries its own `clone_*` / `drop_*` callback pair so
  refcount accounting runs in host-compiled code.
- **`GpuContextFullAccessVTable`** — the privileged surface reachable
  only inside an `escalate(|full| ...)` scope: kernel construction
  (`create_compute_kernel`, `create_graphics_kernel`,
  `create_ray_tracing_kernel`), `acquire_render_target_dma_buf_image`,
  `wait_device_idle`, `acquire_output_texture`,
  `upload_pixel_buffer_as_texture`, `color_converter`,
  `create_command_recorder`, `build_triangles_blas`, `build_tlas`,
  `supports_ray_tracing_pipeline`, `check_in_surface`,
  `create_present_target` + `drop_present_target` (mint / release the
  Box-shaped swapchain present target from a native window handle —
  Linux-only; reserved Win32/AppKit discriminants + non-Linux return the
  typed not-yet-provided refusal). The
  LimitedAccess-mirror methods inherit through the originating
  LimitedAccess vtable rather than duplicating slots here. Each
  callback's `gpu_handle` argument is the opaque scope token issued
  by `escalate_begin`; the host validates the token against
  `escalate_scope_registry::with_scope` before dispatch and returns
  `Error::InvalidEscalateScope` if it's stale.
- **`SurfaceStoreVTable`** — cross-process surface-share daemon ops
  (register, lookup, update layout, unregister) for cdylibs that own
  publishable surfaces.

### PluginAbiObject methods vtables

Per-type vtables that carry the method dispatch for an Arc-handle
PluginAbiObject (the `(handle, vtable, methods_vtable, cached POD)` layout):

- **`TextureRingMethodsVTable`** — `acquire_next`, slot accessor.
- **`VulkanComputeKernelMethodsVTable`** — `bindings`, the various
  `set_*` binding methods, `dispatch`.
- **`VulkanGraphicsKernelMethodsVTable`** — `bindings`, descriptor
  + push-constant binding ops, draw + offscreen render ops.
- **`VulkanRayTracingKernelMethodsVTable`** — kernel-record +
  trace-rays.
- **`VulkanAccelerationStructureMethodsVTable`** — descriptor
  exposure for AS handles consumed by RT kernels.
- **`RhiColorConverterMethodsVTable`** — kernel-prepare hooks
  (`prepare_buffer_to_image_storage` and siblings) the color converter
  exposes for cdylib camera processors.
- **`RhiCommandRecorderMethodsVTable`** — `begin`,
  `record_image_barrier`, `record_buffer_barrier`, `record_dispatch`,
  `record_copy_image_to_buffer`, `submit_signaling_timeline`, plus
  PixelBuffer sibling slots for the cdylib camera per-frame hot path,
  and the swapchain render-path slots the present target's borrowed
  recorder drives (`record_swapchain_image_barrier`,
  `cmd_begin_dynamic_rendering`, `cmd_end_dynamic_rendering`,
  `submit_with_semaphores`, `record_draw` / `record_draw_indexed`).
- **`PresentTargetMethodsVTable`** — `begin_frame` (acquire + prime the
  frame's borrowed recorder), `end_frame` (post-barrier + submit +
  present, folding any producer-finished `extra_waits`), `recreate`
  (swapchain resize / colorspace flip, writing the live
  `out_color_format_raw` so the cached POD getter never goes stale), and
  `set_hdr_metadata`. All per-image render-finished-semaphore keying
  (VUID-vkQueueSubmit2-semaphore-03868) stays host-side across the
  begin/end split. The `PresentTarget` handle is Box-shaped drop-only
  (`!Clone`), backed by a `Box<Mutex<VulkanPresentTarget>>` so a
  concurrent or misordered `begin_frame`/`end_frame` returns a typed
  error, never aliased-`&mut` UB.
- **`OutputWriterVTable`** + **`InputMailboxesVTable`** — per-frame
  `write_raw` / `read_raw` dispatch the host-allocated iceoryx2
  resources hand to the cdylib via `ProcessorVTable::set_iceoryx2_resources`.

### Adapter vtables (separate from the engine plugin ABI)

The surface adapter crates (`streamlib-adapter-vulkan`,
`-opengl`, `-skia`, `-cpu-readback`, `-cuda`) ship their own
adapter-ABI vtables (`VulkanSurfaceAdapterVTable`,
`SkiaSurfaceAdapterVTable`, etc.) that follow the same shape but live
outside `streamlib-plugin-abi`. Each adapter crate carries its own
layout regression tests + a local `run_host_extern_c` panic-catch
wrapper. Adapter ABI is its own audit boundary — see
[`adapter-runtime-integration.md`](adapter-runtime-integration.md)
for the runtime integration shape.

## PluginAbiObject pattern

Every Arc-holding type that crosses the plugin ABI has the same
shape: a fixed `(handle, vtable)` prefix for clone/drop dispatch
plus cached POD fields read by `&self` getters with no plugin ABI hop.
Types that expose methods beyond POD getters (the kernel +
recorder + color converter PluginAbiObjects) carry an additional
`methods_vtable` pointer between the parent vtable and the cached
POD; pure resource-handle PluginAbiObjects (`Texture`, `PixelBuffer`,
buffer types, `TextureRegistration`) skip it.

Reference layout for a POD-only PluginAbiObject (no methods_vtable):

```rust
#[repr(C)]
pub struct Texture {
    /// Opaque handle to host's `Arc<TextureInner>::into_raw()`.
    handle: *const c_void,
    /// Vtable for plugin ABI clone/drop dispatch.
    vtable: *const GpuContextLimitedAccessVTable,
    /// Cached POD fields read by `&self` getters with no plugin ABI hop.
    width_cached: u32,
    height_cached: u32,
    format_raw: u32,
    _padding: u32,
}
```

Reference layout for a method-bearing PluginAbiObject (has methods_vtable):

```rust
#[repr(C)]
pub struct VulkanComputeKernel {
    /// Opaque handle to host's `Arc<VulkanComputeKernelInner>`.
    handle: *const c_void,
    /// Parent vtable for plugin ABI clone/drop dispatch.
    vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for method dispatch.
    methods_vtable: *const VulkanComputeKernelMethodsVTable,
    /// Cached POD fields read by `&self` getters with no plugin ABI hop.
    cached_push_constant_size: u32,
    _reserved_padding: u32,
}
```

Three invariants the pattern locks:

1. **Refcount accounting runs in host-compiled code.** `Clone` calls
   `(*vtable).clone_texture(handle)`; `Drop` calls
   `(*vtable).drop_texture(handle)`. The cdylib never reaches the
   host's `Arc` directly. Both slots short-circuit cleanly on null
   handles.
2. **POD getters read from cached fields.** `texture.width()` returns
   `self.width_cached` with no plugin ABI hop; this is what makes the
   per-frame hot path cheap. The cached fields are populated at
   construction by `from_arc_into_raw` and never mutate over the
   handle's lifetime (the underlying resource is immutable in size /
   format).
3. **Methods dispatch through `methods_vtable`** when the PluginAbiObject
   exposes more than just POD getters. A consumer-side `&self` method
   either reads a cached field (no plugin ABI hop) or calls
   `(*methods_vtable).slot(handle, args...)` (one plugin ABI hop). The
   host-mode and cdylib-mode codepaths produce identical observable
   behavior.

## Mode routing — host vs cdylib

The same PluginAbiObject struct serves both host and cdylib callers. The
deciding factor at runtime is:

```rust
if crate::core::plugin::host_services::host_callbacks().is_some() {
    // We're inside a cdylib (the cdylib's `install_host_services`
    // populated the host_callbacks slot). Dispatch through vtable.
} else {
    // We're the host process. Reach into host-internal layout via
    // host_inner() / vulkan_inner() / buffer_ref().
}
```

The `host_inner()` family of accessors is `pub(crate)` and carries
an explicit panic guard that fires if reached from cdylib code:

```rust
pub(crate) fn host_inner(&self) -> &HostVulkanBuffer {
    if crate::core::plugin::host_services::host_callbacks().is_some() {
        panic!(
            "VertexBuffer::host_inner() reached from cdylib code; this method \
             must dispatch through the GpuContextLimitedAccessVTable."
        );
    }
    unsafe { &*(self.handle as *const HostVulkanBuffer) }
}
```

The panic guard is a defense-in-depth lock; the primary defense is
the `pub(crate)` visibility. A handful of `pub fn`s that take
host-internal types as arguments (raw `vk::CommandBuffer`,
`vk::ImageView`, `vk::AccelerationStructureKHR`) are reachable from
cdylib code by Rust visibility but unreachable by construction
because cdylib code physically cannot mint those types — they only
exist behind `streamlib-engine`'s `vulkanalia` dep, which cdylibs
don't have.

The standard pattern for any new public method that must reach
host-internal state from host code only:

1. Mark the method `pub(crate)` if no cdylib use case exists.
2. If the method MUST be `pub` (a trait method, a public accessor
   used in tests outside the crate), wrap the host-internal access
   in a `host_callbacks().is_some()` panic guard with a message
   naming the method and pointing at the vtable slot that should
   replace it.
3. If the method has a real cdylib use case, add the vtable slot,
   wire the host implementation in `host_services.rs`, route the
   cdylib codepath through the vtable, and write the tier-1
   wire-format tests + the layout regression bump for the affected
   vtable.

The "smuggled parallel" anti-pattern this routing prevents:
duplicating a host-internal accessor as a second `host_inner_full`
helper or an `unsafe_force_dispatch` shortcut instead of going
through the registry. There is ONE way to reach into host-internal
layout (via the guarded `host_inner` family), ONE way to call into
host code from cdylib (via vtable dispatch), and the guards make
the second always visible at compile time when violated.

## The plugin ABI refcount contract

When the host hands a PluginAbiObject across the plugin ABI, the wire
encoding is `Arc::into_raw(inner) as *const c_void`. The cdylib
holds the resulting opaque `handle` and uses it for:

- `Clone` → `(*vtable).clone_*(handle)` — host runs
  `Arc::increment_strong_count(handle)`.
- `Drop` → `(*vtable).drop_*(handle)` — host runs
  `Arc::decrement_strong_count(handle)`, which becomes
  `Arc::from_raw` + drop when refcount hits zero.

The cdylib NEVER constructs an `Arc` directly from the handle. It
never calls `Arc::from_raw`. It never reads the layout of the
inner. The host owns Arc lifecycle end-to-end.

### The `make_*_borrow` trap

The host-side vtable callbacks routinely need to reconstruct a
`ManuallyDrop<PluginAbiObject>` from a `*const c_void` handle the cdylib
passed back to invoke a method on. The `make_*_borrow(handle)`
helpers in `host_services.rs` build that borrow.

**Cached POD fields must be populated on the borrow.** A borrow
constructed with `width_cached: 0` and handed to host-side code
that reads `.width()` returns zero — silently, with no error.
A real bug class: the cdylib pipeline runs end-to-end with zero
errors and produces all-zero output because the host-side
`color_converter::finish_buffer_to_image` read `dst.width()` from
a `make_texture_borrow` that had `width_cached: 0` (see
[@docs/learnings/cdylib-make-borrow-cached-fields.md](../learnings/cdylib-make-borrow-cached-fields.md)).

The canonical pattern is the two-step dance:

```rust
fn make_texture_borrow(handle: *const c_void) -> ManuallyDrop<Texture> {
    // Step 1: minimal borrow, just enough to reach the inner.
    let tex_for_inner = ManuallyDrop::new(Texture {
        handle, vtable: host_vtable(), width_cached: 0, ...
    });
    let hvt = tex_for_inner.vulkan_inner();
    let width = hvt.width();
    let height = hvt.height();
    let format = hvt.format();
    // Step 2: real borrow with cached fields populated from the inner.
    ManuallyDrop::new(Texture {
        handle, vtable: host_vtable(),
        width_cached: width, height_cached: height,
        format_raw: format as u32, _padding: 0,
    })
}
```

The `make_borrow_cached_field_regression_tests` module in
`host_services.rs` locks the contract: each test allocates a real
host-side resource of known dimensions, constructs the borrow, and
asserts the borrow's POD getters return the real values.

## Plugin ABI contracts the ABI commits to

### Wire constants

- `STREAMLIB_ABI_VERSION` — bumped when the wire shape of
  `PluginDeclaration`, the register callback's signature, or
  `HostServices`'s layout changes incompatibly. Plugins must match
  this exactly at load time.
- `HOST_SERVICES_LAYOUT_VERSION` — bumped whenever fields are added
  to or reordered in `HostServices`. Distinct from
  `STREAMLIB_ABI_VERSION` because layout-only additions can ship
  without bumping the wire ABI.
- Per-vtable `*_VTABLE_LAYOUT_VERSION` — one per vtable. The host's
  vtable consumer reads this first and aborts cleanly on mismatch
  rather than dereferencing past-the-end slots.
- `PLUGIN_ABI_LAYOUT_FINGERPRINT` (`streamlib-plugin-abi`) — a
  `const` FNV-1a fold of every wire constant above plus the *measured*
  `size_of` / `align_of` of `PluginDeclaration`, `HostServices`, and
  every vtable struct. Measured (not hand-maintained), so a
  layout-changing edit that forgot to bump the matching version
  constant still changes it — catching a same-constant /
  different-layout republish that a version-only check would miss.
- `ENGINE_TRANSIT_FINGERPRINT` (`streamlib-engine`,
  `core::plugin::build_fingerprint`) — folds
  `PLUGIN_ABI_LAYOUT_FINGERPRINT`, the engine and `streamlib-consumer-rhi`
  crate versions, and (on Linux) the first-order layout of the three
  non-`#[repr(C)]` engine types that transit by raw `Arc`
  (`HostVulkanDevice`, `HostVulkanTexture`,
  `HostVulkanTimelineSemaphore`). The engine version is folded in
  deliberately, so a cross-engine-version load is refused even when
  layouts happen to coincide.

### Load handshake

`PluginDeclaration` (ABI v5) carries a build-fingerprint handshake so a
host refuses — with a typed, actionable error — any plugin whose
`#[repr(C)]` dispatch surface or engine-internal transit surface could
skew from its own. The v5 envelope, pinned by the
`plugin_declaration_layout` regression test:

| offset | field | purpose |
|---|---|---|
| 0 | `abi_version: u32` | pinned; read **first** |
| 4 | `_reserved_padding: u32` | alignment |
| 8 | `register: PluginRegisterFn` | pinned |
| 16 | `abi_layout_fingerprint: u64` | plugin's `PLUGIN_ABI_LAYOUT_FINGERPRINT` |
| 24 | `engine_transit_fingerprint: u64` | plugin's `ENGINE_TRANSIT_FINGERPRINT`, or `0` |
| 32 | `build_identity_ptr: *const u8` | human-readable identity string |
| 40 | `build_identity_len: usize` | identity length |

`export_plugin!` populates the two fingerprints and the identity from
three associated consts the `#[processor]` macro emits against the
detected SDK crate. A **facade plugin** (Cargo-deps `streamlib`,
statically-linking the engine) reports its real
`ENGINE_TRANSIT_FINGERPRINT`; an **engine-free plugin** (Cargo-deps
`streamlib-plugin-sdk`) reports `0`, the "no transit surface" sentinel.

The host's `validate_plugin_declaration` runs three checks in a
load-bearing order, before `register` is invoked:

1. `abi_version == STREAMLIB_ABI_VERSION` — read from the pinned
   offset-0 slot first. A mismatch means the appended v5 fields may not
   exist, so they are **not** dereferenced; returns
   `Error::PluginAbiVersionMismatch`.
2. `abi_layout_fingerprint == PLUGIN_ABI_LAYOUT_FINGERPRINT` — else
   `Error::PluginBuildMismatch`.
3. `engine_transit_fingerprint == 0` (engine-free) **or**
   `== ENGINE_TRANSIT_FINGERPRINT` — else `Error::PluginBuildMismatch`.

Both typed errors name the plugin's and the host's build identities and
the rebuild remedy (publish a matching engine `-dev` version and bump
the plugin's pin, or `streamlib link`). The identity string is read
defensively on the error path — null pointer → `"unknown"`, the length
is capped, and the bytes are lossily decoded — so a corrupt declaration
never drives an unbounded or unsafe read.

**Residual soundness gap:** the transit probe is first-order
(`size_of` / `align_of`), so it catches a transit type whose size or
alignment skews across two builds (the dominant mode) but not a pure
reorder-at-identical-size, which `repr(Rust)` permits. The
engine-version fold narrows the survivable window to "same engine
version, different transitive resolution, same first-order layout"; the
fully sound fix is the PluginAbiObject lift of the remaining raw-`Arc`
slots, which removes the transit entirely.

### Image lifetime — `dlclose` is never called

A dlopen'd plugin image is retained for the **process lifetime**,
unconditionally: on a successful load, on a load that fails after
`register` ran, and on `Runner::remove_module`. Registered
`&'static ProcessorVTable`s, `'static` descriptor strings handed
across the plugin ABI, and the bridge state `install_host_services`
wrote into the cdylib's statics all point into the mapped image —
unmapping it would dangle them behind safe interfaces. "Unloading a
module" therefore means **registration removal** (its processor types
and package-owned schemas leave the host registries; the module loader's
ledger and memo entries clear), never image unmapping. Retained images
are recorded per (path, load) and never deduplicated by path — a
rebuilt plugin at the same path is a new image, and dropping the
"duplicate" handle would `dlclose` a live image out from under its
vtables. A reload after `remove_module` dlopens the path again and
re-runs `register`; `install_host_services` is idempotent, so
re-installing over a retained image is safe.

### What crosses the wire

- C primitives (`u32`, `i32`, `i64`, `u64`, `f32`, `f64`, `bool` as
  `u8`).
- `*const c_void` opaque handles.
- `*const u8` + `usize` length pairs for byte buffers.
- `extern "C" fn` pointers organized into `#[repr(C)]` vtables.
- `#[repr(C)]` structs for descriptor payloads
  (`ComputeKernelDescriptorRepr`, `GraphicsKernelDescriptorRepr`,
  `RayTracingKernelDescriptorRepr`, the binding-spec mirrors,
  viewport / scissor / draw call mirrors, etc.).
- Variable-length structured payloads as msgpack-encoded byte
  buffers, decoded into Rust types on the receiving side
  (`ProcessorDescriptor`, `ProcessorSpec`, `ExecutionConfig`,
  `Event`).

### What does NOT cross the wire

- Rust generic types.
- Trait objects.
- Closures (other than as `extern "C" fn` pointers).
- `Arc<T>` for any non-opaque `T` — only `Arc::into_raw`-encoded
  opaque handles.
- `tokio::runtime::Handle` — the host's runtime is not exposed to
  plugins; plugins own their own tokio runtimes.
- `std::collections::HashMap` and other std collections — encoded
  through msgpack when needed, never as a direct memory view.
- `vulkanalia::Device` / `vk::CommandBuffer` / `vk::ImageView` and
  any other `vulkanalia`-versioned type — these stay host-internal
  by construction (cdylibs don't Cargo-dep `vulkanalia`).

## Panic safety net

Every `extern "C" fn` slot wraps its body in `run_host_extern_c`,
which catches Rust panics with `catch_unwind` and converts them into
a clean error to the cdylib caller — typed by the slot's return
shape (`-2` for `i32` returns, a default-`""` for `&str` returns,
etc.). A panic in a host-side vtable implementation NEVER unwinds
across the plugin ABI; that would be undefined behavior since the
cdylib was compiled with a potentially different panic strategy.

The wrapper itself is locked by the
`run_host_extern_c_panic_safety_net_tests` module in
`host_services.rs`. The mirror principle holds for the cdylib-side
`ProcessorVTable` callbacks (the generic wrapper around the user
processor) and for every adapter crate's local `run_host_extern_c`
copy — each of those carries its own panic-catch path with
weaker direct test coverage than the engine's central wrapper.

## Test discipline

Three categories of tests lock the ABI:

### Layout regression

Every `#[repr(C)]` struct in `streamlib-plugin-abi/src/lib.rs` has
a `#[test]` block that asserts:

- `std::mem::size_of::<T>() == N`
- `std::mem::align_of::<T>() == N`
- For each field, `std::mem::offset_of!(T, field) == N`

The hard-coded numbers are recomputed from the expected struct
shape. A reorder, an inserted field, or a rustc layout change all
break these tests deterministically. A few of the largest vtables
(`HostServices`, `GpuContextLimitedAccessVTable`,
`GpuContextFullAccessVTable`) carry partial coverage — total size
locked but not every offset pinned — and should grow full coverage
over time.

### Tier-1 wire-format

For each vtable callback, an `extern "C"`-side test exercises:

- The positive path (real handle, valid args, returns 0 / Ok).
- Null-handle short-circuit (returns the error code, writes an
  identifying message to `err_buf`).
- Null-out-param short-circuit (callbacks that write through
  `*mut T` out-params short-circuit on null).
- Invalid-args paths (bad UTF-8 in a `*const u8`+`len` string,
  unknown discriminants on tagged enums).
- Error-buffer format (correct error code + populated `err_buf` on
  any failure path).

These tests run without dlopen — they call the static
`HOST_*_VTABLE.slot(...)` pointer directly with synthetic
arguments. Coverage is built up incrementally as vtable callbacks
land.

### Dlopen integration

The `runtime/streamlib-engine/tests/load_project_dylib_*.rs` suite
stages `packages/camera` / `packages/test-fixtures` into a tmpdir and
loads the built cdylib through
`runtime.add_module_with_blocking(ident, Strategy::Path { path, build })`
and exercises the full
ABI roundtrip: cdylib registers via `STREAMLIB_PLUGIN` → host
populates `HostServices` → cdylib instantiates a processor →
runtime drives it through `setup` / `start` / `process` / `stop` /
`teardown`. Each test asserts the lifecycle hits the cdylib in the
expected order.

The dlopen tests are the load-bearing end-to-end gate; null-handle
unit tests catch the structural class of bug, but only dlopen
exercises the lifetime invariants (`Arc::into_raw` →
`Arc::from_raw` symmetry, vtable pointer staying valid across
host_services install) that can only break under a real
cross-process Arc transit.

## Trip-wires

Revisit this doc and the structural decisions when:

1. **A new wire crossing wants to carry a shared Rust type.** Every
   shared-Rust-type crossing eventually becomes a coupling problem
   (the cdylib's view of the type diverges from the host's at the
   first dep-graph or rustc-version skew). Convert to a vtable
   crossing or a msgpack-encoded byte buffer instead.
2. **A new `pub fn` wants to read host-internal layout from cdylib
   code.** This is the gap class — solve it by adding a vtable slot,
   not by smuggling a parallel `host_inner_alt` accessor.
3. **A `#[repr(C)]` struct grows past 64 bytes.** Cache-line
   placement starts to matter; consider whether a field belongs on
   the struct or behind a pointer.
4. **A new vtable would have more than ~30 slots.** Either group
   into per-domain vtables (see the `*MethodsVTable` per-PluginAbiObject
   split) or reconsider whether all slots really need separate plugin ABI
   crossings.
5. **A `make_*_borrow` helper is added.** Mirror the two-step dance
   from the existing helpers (read inner via minimal borrow, then
   construct final borrow with cached fields populated) and add a
   matching test in `make_borrow_cached_field_regression_tests`.
6. **A non-Linux platform grows real cdylib coverage.** The current
   ABI is Linux-rich; macOS / Windows variants of several PluginAbiObject
   methods (Metal command buffer, IOSurface texture, etc.) ship
   stubs to keep the vtable layout unconditional. When a real
   non-Linux consumer arrives, those stubs need to grow real
   implementations.

## Reference

- **ABI crate**: `runtime/streamlib-plugin-abi/src/lib.rs` — every
  `#[repr(C)]` shape, every layout version constant, every layout
  regression test.
- **Host-side implementations**:
  `runtime/streamlib-engine/src/core/plugin/host_services.rs` — every
  vtable callback impl, every `make_*_borrow` helper, the
  `run_host_extern_c` panic safety net, the
  `make_borrow_cached_field_regression_tests` module.
- **Cdylib-side dispatch shims**: `runtime/streamlib-engine/src/core/`
  for the PluginAbiObjects (`rhi/texture.rs`, `rhi/pixel_buffer.rs`,
  `rhi/storage_buffer.rs`, etc.) and per-type method dispatch
  (`vulkan/rhi/vulkan_compute_kernel.rs`,
  `vulkan/rhi/vulkan_graphics_kernel.rs`, etc.). Each carries the
  `host_callbacks().is_some()` branch that picks vtable vs host
  dispatch.
- **Consumer carve-out**:
  `runtime/streamlib-consumer-rhi/src/consumer_vulkan_device.rs` plus
  siblings for the import-side Vulkan surface cdylibs ride.
- **SDK façade**: `sdk/streamlib-sdk/src/lib.rs` for the safe
  surface cdylibs Cargo-dep through.
- **Reference cdylib**: `examples/camera-rust-plugin/` — the
  in-tree end-to-end smoke example of a facade cdylib plugin. (The
  dlopen integration tests themselves stage `packages/camera` /
  `packages/test-fixtures`, not this example.)
- **Companion docs**:
  - [`adapter-runtime-integration.md`](adapter-runtime-integration.md) —
    how surface adapters ride this ABI to expose host-allocated
    resources to cdylib customers.
  - [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) — which
    RHI patterns the cdylib re-implements (the import-side
    carve-out) vs escalates through this ABI.
  - [@docs/learnings/cdylib-make-borrow-cached-fields.md](../learnings/cdylib-make-borrow-cached-fields.md) —
    the make_*_borrow cached-field trap and how to avoid it.
