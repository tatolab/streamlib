# `TextureRegistration` — engine-wide per-surface lifecycle state

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

## What this is

`TextureRegistration` is the **single canonical engine-wide record for
per-surface lifecycle state**, keyed by `surface_id` in
`GpuContext::texture_cache`. Each registration carries the texture
plus the typed mutable fields that producers and consumers both need
to read/write across the surface_id handoff:

- `texture: Texture` — the resource itself.
- `current_layout: AtomicI32` (Linux only — stores
  `streamlib_consumer_rhi::VulkanLayout`) — the last-known Vulkan
  image layout. Producers update on transitions; consumers read for
  barrier source layouts.

Additive fields go on this record (see [What goes in / what stays
out](#what-goes-in--what-stays-out) below). The shape mirrors the
per-surface state pattern surface adapters already use (every
in-tree adapter — vulkan, opengl, cuda, cpu-readback — carries its
own adapter-scope `SurfaceState<P>`) — `TextureRegistration` is the
same shape lifted from adapter-scope to engine-wide scope.

## Why it exists

The engine-model fix (per
[CLAUDE.md "Engine-wide bugs get fixed at the engine
layer"](../../CLAUDE.md#core-operating-principles--read-first)) makes
the handoff contract between producers and consumers **explicit and
typed at the engine layer**. The alternative — implicit conventions
encoded in producer/consumer code — breaks the moment a new producer
ships a texture in a layout the consumer doesn't expect (the
descriptor binding claims a layout that doesn't match reality;
NVIDIA tolerates the mismatch silently, Vulkan validation layers
warn). Adapter-scoped `Registry<SurfaceState>` records are visible
only to adapter code, so they can't carry handoff state for
disjoint producers and consumers that go through `texture_cache`.
`TextureRegistration` is the engine-scope record that closes that
gap.

## Scope: this record vs. adapter-internal `SurfaceState<P>`

`TextureRegistration` (engine-wide, in `GpuContext::texture_cache`)
and the adapter crates' `SurfaceState<P>` records (in
`streamlib-adapter-vulkan`, `-opengl`, `-cuda`, `-cpu-readback`)
are at **different scopes by deliberate design**, not redundant
maps. They look superficially similar (both keyed by `surface_id`,
both can carry a `current_layout` field) but hold different state
for different consumers:

| Scope | Record | What it carries | Who reads it |
|---|---|---|---|
| Engine-wide | `TextureRegistration` | Same-process handoff state for disjoint producers/consumers (camera → display, OpenGL adapter → display): texture handle, last-known layout. | In-tree pipeline code via `resolve_texture_registration_by_surface_id`. |
| Adapter-internal | `SurfaceState<P>` | Adapter's acquire/release state machine: `read_holders`, `write_held`, timeline values, framework-specific handles (e.g. EGL image, GL texture id, CUDA external memory mapping). | Only the adapter's own `acquire_*` / `release_*` paths. |

This mirrors Unreal Engine 5's deliberate scope split between
`FRDGSubresourceState` (per-pass handoff state read by the next
consumer) and `FPooledRenderTarget` (allocator-internal pool /
refcount state) — two records, two scopes, zero conflation. To the
best of our current knowledge the same shape applies in Granite
(typed identity record on `Vulkan::Image` plus per-pass transient
state in the render graph) and The-Forge (persistent props on
`Texture`, transitional state on call-site `TextureBarrier`).
Verify against current code at pickup if revisiting.

The Anti-pattern #1 rule below (no parallel `HashMap<surface_id, …>`)
applies **within scope**: don't create a second engine-wide map
alongside `texture_cache`, and don't create a second adapter-
internal map alongside an adapter's `Registry`. Different-scope
records that share a key but hold disjoint field sets and serve
disjoint consumers are **not** the failure mode the rule exists
to prevent.

## How it works

```
producer:                  consumer:
  register_texture_with_      resolve_texture_registration_
    layout(id, tex, L)          by_surface_id(...)
       │                            │
       ▼                            ▼
  ┌─────────────────────────────────────────┐
  │ GpuContext::texture_cache               │
  │   HashMap<surface_id, Arc<TexReg>>      │
  │     ├── texture: Texture                │
  │     └── current_layout: AtomicI32       │
  └─────────────────────────────────────────┘
       ▲                            │
       │                            ▼
  update_layout(L′) after     barrier(old=current_layout,
    a producer transition       new=target_layout);
                              update_layout(target_layout);
```

The `Arc<TextureRegistration>` is shared across all holders in-process
— no IPC, no schema changes. `current_layout` reads use
`Ordering::Acquire`; writes use `Ordering::Release`. Multi-consumer
races are tolerated (see [Race model](#race-model)).

## What goes in / what stays out

### In: state both producers and consumers care about for handoff

A field belongs in `TextureRegistration` when **all three** are true:

1. The field's value is set by the producer and read by the
   consumer (or vice-versa) — the handoff is the contract this
   record encodes.
2. The field has a stable identity tied to the surface_id (lives
   from registration through unregistration) — it's not transient
   per-frame data.
3. Both producers and consumers run on the same in-process address
   space, OR the cross-process IPC layer round-trips the field
   (see [Cross-process coordination](#cross-process-coordination)).

Examples that fit:

- ✓ `current_layout` — present today.
- ✓ `last_writer_id` — debugging "where did this surface come from?";
  set on registration, read by anyone tracing the pipeline.
- ✓ `last_written_frame_index: AtomicU64` — staleness detection;
  producer increments per write, consumer compares to its expected
  frame.
- ✓ `format`, `width`, `height` — could be hoisted from `Texture`
  for cheaper validation; arguably already covered by `texture`.
- ✓ Exportable timeline-semaphore handles — for consumers that need
  to GPU-wait without a side-channel. Subprocess-wired adapter
  surfaces today carry two timelines per surface (`produce_done` +
  `consume_done`, one per direction of the producer ↔ consumer
  edge); see
  [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
  for the single-writer-per-edge contract.

### Out: state that doesn't fit any one of the criteria

- ✗ **Adapter-internal state** (e.g., the OpenGL adapter's cached
  `EGLImage` + GL texture id, or the cuda adapter's `cudaExternalMemory`
  handle). Stays in the adapter's own `SurfaceState`. Engine doesn't
  need to see it; other consumers don't either.
- ✗ **Per-frame transient state** (the timestamp of *this* frame, the
  encoder's `is_keyframe` bit, etc.). These belong on the `VideoFrame`
  IPC message, not on the surface registration. The registration
  outlives any single frame.
- ✗ **RDG-style declared-usage hints** ("I'll read this as a SAMPLED_READ
  in pass N+1"). Adding declared usage here without a graph compiler
  to interpret it is just dead metadata that producers and consumers
  can both lie about.
- ✗ **Cross-process-only state without a same-process consumer.** Use
  the surface-share daemon's own state or extend its IPC schema. The
  `texture_cache` is for in-process consumers reaching textures via
  `resolve_texture_registration_by_surface_id` Path 1.

When you find yourself wanting to add a field that doesn't fit cleanly,
**stop and ask**:

- "Is this really per-surface, or is it per-frame?" → if per-frame, use
  `VideoFrame` IPC fields.
- "Does the engine need to see this, or only the adapter?" → if only
  the adapter, keep it in `SurfaceState`.
- "Am I tracking declared usage?" → if so, that's automatic-barrier-
  inference territory; it lives in a layer above this one, not on
  `TextureRegistration` itself.

## Producer rules

1. **Declare your post-publish layout at registration time.**
   ```rust
   gpu.register_texture_with_layout(
       &surface_id,
       texture,
       VulkanLayout::SHADER_READ_ONLY_OPTIMAL,  // or whatever you actually leave it in
   );
   ```
   The "post-publish" layout is the layout the texture is in **at the
   moment any consumer dereferences the surface_id**. For producers
   that write IPC frames, this is the layout immediately after the
   transition that precedes the IPC write.

2. **Use the back-compat shim only when the layout is genuinely
   `UNDEFINED`.**
   ```rust
   gpu.register_texture(&id, texture);  // defaults to UNDEFINED
   ```
   This is correct for a freshly-allocated texture that no one has
   touched (e.g. `acquire_output_texture`). It is **not** a "I don't
   know, just pick something" escape hatch. If you actually do know
   the layout, declare it. If you genuinely don't, that's a code smell
   — the texture should have a knowable post-allocation state.

3. **Update on mid-pipeline transitions.**
   ```rust
   // After issuing a barrier that moves the texture to a new layout:
   reg.update_layout(VulkanLayout::TRANSFER_SRC_OPTIMAL);
   ```
   Producers that transition the texture multiple times during a frame
   only need to update the field after the *last* transition before the
   IPC publish — intermediate layouts aren't observed by consumers.

4. **Don't lie.** A registration that claims `SHADER_READ_ONLY_OPTIMAL`
   when the Vulkan tracker is in `GENERAL` re-creates the exact bug
   `TextureRegistration` exists to fix. If your producer doesn't
   transition the Vulkan tracker (e.g., the OpenGL adapter, which
   writes via GL without issuing Vulkan barriers), declare what's
   actually true: `UNDEFINED` initially, then whatever the last
   consumer's barrier left it in (read it back from the registration
   if the producer needs to reason about it).

5. **Dual-register when the surface flows to both subprocess and
   in-process hot-path consumers.** Adapter `install_setup_hook`
   wirings that publish a surface to `surface_store` (cross-process)
   AND have a same-process consumer reading it every frame must also
   call `register_texture_with_layout` — Path 2 explicitly does not
   cache its synthesized registration, so a Path-1 miss on the hot
   path costs a fresh DMA-BUF import + QFOT acquire submit per frame.
   See [`adapter-runtime-integration.md` →
   Dual-registration](adapter-runtime-integration.md#dual-registration-for-in-process-consumers)
   for the recipe and the in-tree reference producer.

## Consumer rules

1. **Resolve the registration, not just the texture.**
   ```rust
   let reg = gpu.resolve_texture_registration_by_surface_id(
       surface_id,
       texture_layout,
       width,
       height,
   )?;
   let texture = reg.texture();
   let current = reg.current_layout();
   ```
   `resolve_texture_by_surface_id` is the thin projection for callers
   that don't need the metadata — but if you're issuing a barrier, use
   the registration form.

2. **Barrier from `current_layout` to your target.**
   ```rust
   if current != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
       device.cmd_pipeline_barrier2(cmd, &dep_info(/* old=current, new=target */));
       reg.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
   }
   ```
   Skip the barrier when source equals target — Vulkan permits no-op
   transitions but they emit validation warnings on some drivers.

3. **Update after the barrier records.** The
   `update_layout` call should be the source-of-record for the next
   reader; race tolerance is documented but not magic. If you skip the
   update after a real transition, the next consumer will issue a
   barrier from the wrong source layout.

4. **Don't cache `current_layout` across frames.** A future frame may
   have a different producer, or the layout may have been changed by
   another consumer. Read it fresh per frame.

## Anti-patterns

These are the failure modes the engine-model rule exists to prevent.
Each was either tried and rejected, or is the foreseeable workaround
that future agents would attempt without this doc.

1. **Parallel `HashMap<surface_id, FooState>` *within scope*.** If
   you find yourself wanting to track new engine-wide per-surface
   metadata in a separate HashMap alongside `texture_cache`, stop.
   Extend `TextureRegistration`. The whole point of the engine-wide
   cache is that there's *one* keyed registry at engine scope;
   multiple parallel engine-wide ones recreate the implicit-
   convention problem one layer up. (See
   [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
   above — adapter-internal `SurfaceState<P>` lives at a different
   scope and does not violate this rule.)

2. **Descriptor-side claims that don't match registration.** Claiming
   `SHADER_READ_ONLY_OPTIMAL` in a `VkDescriptorImageInfo::imageLayout`
   field while the actual texture is in `GENERAL` is the failure mode
   this record exists to prevent. The fix is to barrier first, not to
   claim something else and hope the driver tolerates it. Never use
   the descriptor's `imageLayout` field as a workaround for an unknown
   source layout — barrier the texture into the layout you're going
   to claim.

3. **Side-channel "I know better" code paths.** If a consumer thinks
   it knows the source layout better than the registration claims,
   the answer is to fix the producer's declaration, not to ignore the
   registration and barrier from a hardcoded layout. The registration
   is the source of truth; if it's wrong, fix it at the source.

4. **Reaching through private fields to bypass the API.** No
   `_lib._texture_cache._inner` reach-through code paths. If you need
   the registration's internals, add a public method on
   `TextureRegistration` itself.

5. **"Just unconditionally barrier from `GENERAL`"** as a way to dodge
   tracking. Trades one validation warning class for another. The
   right answer is typed tracking.

6. **Adding declared-usage hints onto `TextureRegistration`.** "I'll
   need this as SAMPLED_READ next" is automatic-barrier-inference-shape
   information; without a graph compiler to read it, it's dead
   metadata that producers and consumers can both lie about. The
   substrate `TextureRegistration` provides (typed per-surface state
   keyed by stable id) is the right shape for an eventual barrier-
   inference layer to build on top of — not the place to bolt
   declared-usage hints onto.

## Race model

`current_layout` is `AtomicI32` with `Acquire` reads and `Release`
writes. The Arc itself is `Send + Sync`. Consequences:

- **Single producer + single consumer** (the dominant pattern today —
  one camera writes, one display reads): clean, layout claims always
  match GPU reality after the consumer's barrier records.
- **Single producer + multiple consumers in the same frame** (e.g.
  display + encoder both consuming a camera ring texture): the second
  consumer's `current_layout` read may see the first consumer's
  updated value, in which case it issues a no-op barrier; or it may
  race and both consumers issue barriers from the same source layout,
  in which case the queue mutex serializes the submissions and the
  GPU work is correct. **Documented as race-tolerant**, not race-free.
- **Multiple producers in the same frame**: not a supported pattern.
  `register_texture_with_layout` overwrites the registration; if a
  producer needs to share a surface with another producer it has to
  coordinate out-of-band.

The race model is good enough today because streamlib pipelines are
predominantly single-consumer-per-frame. Multi-consumer coordination
beyond what queue serialization provides would belong in an
automatic-barrier-inference layer above this one, where coordination
is the engine's job rather than the consumer's.

## Cross-process coordination

`TextureRegistration` is the same-process record. Cross-process
producers and consumers coordinate layout via three layers, in
priority order at consumer-side resolution:

1. **Per-frame `VideoFrame.texture_layout` (optional)** — for
   producers that vary layout per frame. Carried on the IPC message
   itself; serialized as the raw `int32 VkImageLayout` enumerant;
   absent when the producer relies on the per-surface default.
2. **Per-surface `current_image_layout`** — declared by the producer
   at `surface_store.register_texture` time and refreshed via
   `surface_store.update_image_layout` after each post-write release.
   Carried in surface-share IPC `register` / `lookup` /
   `update_layout` messages.
3. **Default `UNDEFINED`** — back-compat for surface-share daemons /
   producers that haven't published a layout; the consumer's acquire
   barrier short-circuits as a no-op when target is UNDEFINED.

The host consumer's `GpuContext::resolve_texture_registration_by_surface_id`
Path 2 reads (1) when present, falls back to (2), and runs
`HostVulkanDevice::acquire_from_foreign` with the resolved layout.
The producer-side equivalent is `HostVulkanDevice::release_to_foreign`
(or `VulkanSurfaceAdapter::release_to_foreign` for adapter-mediated
producers); the cdylib-side mirrors live on `ConsumerVulkanDevice`
and the consumer-rhi `VulkanRhiDevice` trait so adapters generic over
device flavor work unchanged.

### QFOT vs bridging fallback

`acquire_from_foreign` chains `VkExternalMemoryAcquireUnmodifiedEXT`
when `VK_EXT_external_memory_acquire_unmodified` is enabled at device
construction (probed by `HostVulkanDevice::new` /
`ConsumerVulkanDevice::new`); this is the spec-correct
content-preserving QFOT acquire. The queue family index used for
QFOT src/dst is `VK_QUEUE_FAMILY_EXTERNAL`, which is core Vulkan 1.1
(promoted from `VK_KHR_external_memory`) and always available — only
the optional acquire-unmodified extension is the meaningful gate.

When the optional extension is missing the helper falls back to a
bridging `UNDEFINED → target` transition. Content discard is
permitted by spec for this transition, but DMA-BUF kernel-side memory
contents are preserved in practice on every modern Linux Vulkan
driver (NVIDIA confirmed empirically through streamlib's E2E
camera→Path-2-display flow; Mesa iris/radeonsi follow the same
convention).

**The bridging fallback is structurally permanent on NVIDIA Linux,
not interim.** To the best of our current knowledge,
`VK_EXT_external_memory_acquire_unmodified` is not on NVIDIA's
roadmap even though NVIDIA engineers contributed to the extension.
NVIDIA exposes adjacent extensions (`VK_EXT_external_memory_dma_buf`,
`VK_EXT_external_memory_host`) but not the acquire-unmodified one.
Cross-process content preservation on NVIDIA therefore depends on
the empirical DMA-BUF kernel-cache behavior, not the spec. Mesa is
the eventual landing point for the QFOT-acquire path; NVIDIA
consumers ride the bridging fallback indefinitely.

### Producer-side adoption is incremental

The producer-side QFOT release machinery
(`HostVulkanDevice::release_to_foreign` and
`VulkanSurfaceAdapter::release_to_foreign`) is in place; in-tree
producers and cdylib FFI release paths adopt it as their
cross-process correctness story requires. Adapter-cuda and
-cpu-readback are out of scope by construction (CUDA imports use
`cudaImportExternalMemory` ownership semantics; cpu-readback is
buffer-only with no Vulkan layout).

### Why no sandbox-side mirror

Three independent lines of evidence point the same way:

1. **Vulkan spec.** `VkImageCreateInfo::initialLayout` must be
   `UNDEFINED` or `PREINITIALIZED`. There is no "import this
   in layout L" form. Every freshly-created `VkImage` in the
   consumer process — even one bound to imported memory — starts
   at its declared `initialLayout`. The consumer's layout state
   is its own state machine, **independent of the producer's by
   spec construction**, not a stale view of it.

   Khronos's `VK_EXT_external_memory_acquire_unmodified`
   proposal states this directly: *"The solution should not
   require the implementation to internally track the
   `VkImageLayout` of external images, as such tracking can be
   complex to implement and cause performance overhead."*
   Cross-process layout is communicated by **application
   protocol** (release/acquire barriers via
   `VK_QUEUE_FAMILY_EXTERNAL`, or in our case the IPC schema),
   not by shared mutable state.

2. **Sandboxed-host architecture precedent.** Every closely-
   analogous system mirrors only **immutable descriptor metadata**
   (size, format, usage) on the sandbox side; mutable lifecycle
   state stays server-side. To the best of our current knowledge:

   - Dawn Wire client `Texture.h` caches descriptor shape only;
     no `current_layout`, no barrier scope.
   - Chromium SharedImage hands renderers an opaque 16-byte
     `gpu::Mailbox` with **zero embedded metadata**; the backing
     lives in the GPU process.
   - wgpu-core's `TextureTracker` / `TextureUsageScope` live in
     the hub (server); user-facing `wgpu` holds only
     `Arc<Texture>` resource handles.
   - WebGPU spec §3.4 explicitly splits "content timeline"
     (client-visible, immutable descriptor) from "device
     timeline" (server-side, mutable, accessed asynchronously
     via `GPUError`).

   Verify the patterns against current upstream source if
   revisiting.

3. **In-tree evidence.** No subprocess code constructs
   `TextureRegistration` today. Cdylibs use
   `HostSurfaceRegistration<P>` (an adapter-scope record at the
   adapter crate, generic over `DevicePrivilege`) plus the
   `VulkanLayout` enum (in `streamlib-consumer-rhi`).
   The "subprocess needs typed-contract for layout" concern is
   covered at the adapter scope, where it belongs per
   the [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
   section.

The architecturally correct cross-process work is the **IPC
schema lift**: the producer's *published layout* travels in the
surface-share / `VideoFrame` schema as a typed protocol field;
the host consumer reads it once at acquire time and barriers from
there. No mirror, no shared mutable record across the boundary.

## Tests

When a new field lands on `TextureRegistration`:

1. **Unit test in `texture_registration.rs::tests`**: exercise
   round-trip (set → read), concurrent updates from N threads (no
   torn values), and any field-specific invariants.
2. **Unit test in `gpu_context.rs::tests`**: exercise the resolve
   path — register with the new field, resolve via
   `resolve_texture_registration_by_surface_id`, assert visibility.
3. **Mentally revert the field's update logic** — does the test still
   pass? If yes, the test is feel-good and doesn't lock the contract.
   Strengthen it: a test that doesn't fail when the impl is reverted
   isn't locking anything.
4. **E2E with `VK_LOADER_LAYERS_ENABLE=*validation*`** for any field
   that affects GPU behavior. The unit tests lock the data structure;
   validation-layer E2E locks the actual Vulkan-side correctness.

## Reference

- **Implementation**: `libs/streamlib-engine/src/core/context/texture_registration.rs`,
  `GpuContext::register_texture_with_layout` /
  `GpuContext::resolve_texture_registration_by_surface_id` in
  `libs/streamlib-engine/src/core/context/gpu_context.rs`.
- **First consumer**: `LinuxDisplayProcessor::render_frame` in
  `packages/display/src/linux/display.rs`.
- **First adapter-output producer**: `register_render_target_surface`
  in `examples/camera-python-display/runner/src/linux.rs`.
- **First in-tree producer**: `LinuxCameraProcessor` in the
  `streamlib-camera` package —
  `packages/camera/src/linux/camera.rs`.
- **Adapter-scope sibling**: `SurfaceState` in
  `libs/streamlib-adapter-vulkan/src/state.rs` (and the same-shape
  opengl + cuda + cpu-readback adapter state structs). These are at
  adapter scope, **not** parallel maps to `texture_cache` — see the
  [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
  section.
- **Adapter timeline contract**:
  [`adapter-timeline-single-writer.md`](adapter-timeline-single-writer.md)
  — single-writer-per-edge contract for the `produce_done` +
  `consume_done` timeline pair every subprocess-wired adapter
  surface carries.
- **External references**:
  - [Khronos `VK_EXT_external_memory_acquire_unmodified` proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html)
  - [Vulkan synchronization & queue-transfer chapter](https://docs.vulkan.org/spec/latest/chapters/synchronization.html)
  - [Dawn wire client Texture.h](https://dawn.googlesource.com/dawn/+/refs/heads/main/src/dawn/wire/client/Texture.h)
  - [Chromium SharedImageBacking](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/gpu/command_buffer/service/shared_image/shared_image_backing.h)
  - [wgpu-core track/texture.rs](https://github.com/gfx-rs/wgpu/blob/trunk/wgpu-core/src/track/texture.rs)
  - [WebGPU spec §3.4 (Programming Model)](https://www.w3.org/TR/webgpu/)
  - [UE5 `FRDGSubresourceState`](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Runtime/RenderCore/FRDGSubresourceState/IsUsedBy)
