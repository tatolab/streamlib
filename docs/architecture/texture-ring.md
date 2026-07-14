# Texture rings — single canonical abstraction

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

## What this is

`TextureRing` is streamlib's **single canonical abstraction for
pre-allocated, per-frame-rotating output textures** on the decode /
CPU-upload hot path. One ring per source; `MAX_FRAMES_IN_FLIGHT = 2`
slots is the standard depth. Construction is privileged
(`GpuContextFullAccess::create_texture_ring`, allocates `count`
non-exportable DEVICE_LOCAL textures and registers each in the
same-process texture cache); per-frame rotation
(`TextureRing::acquire_next`) is sandbox-safe and never escalates.
The companion Limited primitive
`GpuContextLimitedAccess::copy_pixel_buffer_to_texture` writes
CPU-staged bytes into a ring slot's pre-allocated texture without
escalation — the queue submit goes through the shared
mutex-protected command queue.

This is engine-model territory ([CLAUDE.md "The StreamLib Engine
Model"](../../CLAUDE.md#the-streamlib-engine-model)). The shape
exists once; every decode-output / CPU-upload-style hot path uses
it. Hand-rolling a parallel ring inside a consumer is the
anti-pattern this abstraction exists to prevent.

For GPU-native producers that write to ring slots via a compute
kernel (camera, future GPU-decoded JPEG / H.264 / H.265 output)
the same ring shape applies — the only difference is whether the
consumer fills a slot via `copy_pixel_buffer_to_texture` (CPU
upload) or by dispatching its own compute kernel directly against
the slot's `texture`. The ring doesn't care.

## What the abstraction does for you

Given `(width, height, format, usages, count)`, the ring:

1. **Allocates `count` non-exportable DEVICE_LOCAL textures** via
   the host RHI's `create_texture_local` path. Skips the DMA-BUF
   export pool, so the textures don't eat into NVIDIA Linux's
   per-process DMA-BUF allocation cap (per
   `docs/learnings/nvidia-dma-buf-after-swapchain.md`) — that
   budget is reserved for textures that genuinely cross a process
   boundary.
2. **Mints a stable per-slot `surface_id`** (UUID) and registers
   each slot in [`GpuContext`]'s same-process texture cache
   (Path 1 lookup; see
   [`texture-registration.md`](texture-registration.md)) with
   `current_layout = UNDEFINED` — spec-correct for a
   freshly-allocated `VkImage` per
   [`texture-registration.md`](texture-registration.md)'s Producer
   Rule 2. After the first per-frame `copy_pixel_buffer_to_texture`
   runs on a slot, the layout updates to
   `SHADER_READ_ONLY_OPTIMAL` (the layout
   `upload_buffer_to_image` leaves the image in) and stays there
   for the steady-state hot path. Downstream consumers that
   resolve a slot's surface_id always do so AFTER the producer
   has published it onto a `VideoFrame`, which by construction
   happens AFTER the per-frame copy — so the registration and
   reality always agree at the moment any consumer actually reads.
3. **Returns an `Arc<TextureRing>`** holding the slots. The
   processor stores the `Arc` on its struct; `acquire_next()` is
   `&self` and Limited-safe.
4. **Unregisters every slot's `surface_id` on `Drop`** so the
   texture cache doesn't outlive the underlying textures.

The per-frame hot path is the ring's `copy_pixel_buffer_to_slot`
method: write a host-visible pixel buffer's contents into the
slot's *already-allocated* device-local texture via
`vkCmdCopyBufferToImage`, transitioning UNDEFINED → TRANSFER_DST →
SHADER_READ_ONLY_OPTIMAL. The UNDEFINED source layout discards
prior contents — exactly what a rotating ring wants (the slot's
previous contents are about to be overwritten anyway). After the
upload, the registration's `current_layout` is refreshed to
SHADER_READ_ONLY_OPTIMAL to match reality.

Critically, the upload command pool + command buffer + fence are
**pre-allocated per slot** at ring construction
(`HostVulkanUploadResources` — one private command pool per slot
with `RESET_COMMAND_BUFFER_BIT`, plus one `VkCommandBuffer` and
one signaled-at-construction `VkFence`). Per-frame the path is
just: `vkResetFences` + `vkResetCommandBuffer` + record (begin +
barriers + copy + barriers + end) + `vkQueueSubmit2` + `vkWaitForFences`.
No `vkCreateCommandPool`, no `vkAllocateCommandBuffers`, no
`vkCreateFence`, no destroy.

The generic [`GpuContextLimitedAccess::copy_pixel_buffer_to_texture`]
primitive — non-amortized, allocates+destroys resources per call —
stays as an escape hatch for non-ring callers that genuinely don't
have a ring (one-off uploads, test fixtures). Ring consumers
always use `copy_pixel_buffer_to_slot`.

### Honest accounting of what's eliminated

What pre-allocation + amortization eliminate from the per-frame
hot path:

- `GpuContextLimitedAccess::escalate(...)` — the
  `processor_setup_lock` mutex acquire (contended across all
  processors doing setup-class work concurrently — encoder +
  decoder + display all touching `escalate` is a real bottleneck)
- `vkDeviceWaitIdle()` at the end of every escalate — a **full
  device sync** that waits for every queue to drain
- `create_texture_local(...)` — the heavyweight `vkAllocateMemory`
  + `vkCreateImage` + `vkBindImageMemory` triple (the largest
  driver-side cost of the old shape, and the one that ate the
  NVIDIA per-process DMA-BUF cap)
- `vkCreateCommandPool` + `vkAllocateCommandBuffers` +
  `vkCreateFence` + matching destroy quartet per call (replaced
  by `vkResetFences` + `vkResetCommandBuffer` reuse against
  per-slot pre-allocated resources)

What remains per-frame on the upload path:

- `vkResetFences` + `vkResetCommandBuffer` (cheap — driver-internal
  state flip; no allocator hits)
- `vkBeginCommandBuffer` + record barriers + `cmd_copy_buffer_to_image`
  + record barriers + `vkEndCommandBuffer` (constant cost — CPU
  recording overhead, ~microseconds)
- `vkQueueSubmit2` through the shared queue mutex (single submit;
  the only contention is with concurrent submits from other
  processors using the same queue)
- `vkWaitForFences(u64::MAX)` on the slot's own fence (waits for
  this one submit to complete on the GPU; bounded by the actual
  copy time — typically tens of microseconds at decode resolutions,
  vs `vkDeviceWaitIdle` which waits for ALL pending GPU work
  across every queue)

A further future optimization is async-upload: don't wait at
submit, emit the VideoFrame with a timeline value, have downstream
consumers wait on the timeline. That's a model change (sync
pipeline → async pipeline) and a separate engine concern.

## Adding a new consumer — the recipe

### CPU-upload consumers (decoded RGBA bytes → GPU)

1. **Add `Option<Arc<TextureRing>>` to the processor's state.**

2. **Build the ring at setup-time** when dimensions are known up
   front (e.g. a file source whose config carries `width`/`height`):

   ```rust
   let ring = ctx.gpu_full_access().create_texture_ring(
       width,
       height,
       TextureFormat::Rgba8Unorm,
       TextureUsages::COPY_DST
           | TextureUsages::TEXTURE_BINDING
           | TextureUsages::STORAGE_BINDING,
       /* count */ 2,
   )?;
   ```

   **Or build it lazily on the first decoded frame** when
   dimensions are only known after parsing (e.g. H.264/H.265 from
   SPS):

   ```rust
   let need_rebuild = match self.texture_ring.as_ref() {
       Some(ring) => ring.width() != width || ring.height() != height,
       None => true,
   };
   if need_rebuild {
       self.texture_ring = Some(gpu_ctx.escalate(|full| {
           full.create_texture_ring(width, height, format, usages, RING_DEPTH)
       })?);
   }
   ```

   Resolution-change handling is just "drop and rebuild via
   escalate" — the one acceptable per-resolution-change
   escalation. SPS-driven dimensions are stable within a session,
   so this fires at most once per stream in practice.

3. **Per-frame: rotate + copy.** No escalation.

   ```rust
   let slot = self.texture_ring.as_ref().unwrap().acquire_next();
   gpu_ctx.copy_pixel_buffer_to_texture(
       &pixel_buffer,
       &slot.texture,
       &slot.surface_id,
       width,
       height,
   )?;
   let video_frame = VideoFrame {
       surface_id: slot.surface_id,
       width,
       height,
       /* … */
   };
   ```

4. **Drop the ring in `teardown` / `stop`.** The `Drop` impl
   unregisters slot entries from the texture cache.

### GPU-native producers (camera, future JPEG decoder, etc.)

Same ring construction (privileged, setup-time). Per-frame:

```rust
let slot = self.texture_ring.acquire_next();
// Dispatch your compute kernel / hardware decode writing directly
// to slot.texture, then ship `slot.surface_id` downstream.
my_kernel.set_storage_image(0, &slot.texture)?;
my_kernel.dispatch(gx, gy, gz)?;
```

The Limited copy primitive isn't needed in this shape — the
producer writes via its own GPU path. The slot's
`current_layout` registration claim is consumer-managed; producers
that leave the slot in a layout other than
SHADER_READ_ONLY_OPTIMAL must update the registration via
`TextureRegistration::update_layout` after their final transition
(see [`texture-registration.md`](texture-registration.md) for the
contract).

## What's deliberately not covered

- **Cross-process ring sharing.** Same-process consumers only. A
  producer that needs to ship texture slots across the IPC
  boundary uses the surface-share registry path
  (`gpu.surface_store().register_texture(...)`) per
  [`adapter-runtime-integration.md`](adapter-runtime-integration.md);
  the engine ring is for in-process producer→consumer handoffs
  via the texture cache (Path 1).
- **Render-target rings for the display swapchain.** Display
  manages its own per-image render-finished semaphores keyed by
  `image_index` from `acquire_next_image_khr`
  (`docs/learnings/vulkan-frames-in-flight.md`); the swapchain
  ring isn't a `TextureRing`, it's part of the swapchain
  abstraction.
- **Adapter-side per-port intermediates.** The blending
  compositor's `normalize_layer` cache is keyed by input port
  (a `HashMap<port, Texture>`) and reallocates on dim change,
  which is a different shape than a flat-N rotating ring. If
  this pattern shows up in another adapter, file a separate
  issue — a keyed/resizable ring is a meaningful API growth, not
  an obvious extension.
- **Concurrent writes against the same slot.** `acquire_next`
  rotates monotonically and the GPU work the consumer submits
  through the shared queue mutex is serialized correctly, but a
  caller that hands the same slot to two parallel threads
  expecting both writes to land is on their own. Hold one ring
  per writer.
- **Slot-write completion gating.** `acquire_next` doesn't wait
  for prior GPU work on the slot to retire — that's what
  `MAX_FRAMES_IN_FLIGHT = 2` plus the GPU's natural pipeline
  depth gives you. Callers that need explicit slot-retire sync
  (e.g. for a deeper pipeline) layer a timeline semaphore on top.

## Why this shape

Engine-model rule: the RHI is the single gateway for GPU work;
core systems live once and grow via extension, never via parallel
implementations ([CLAUDE.md "Before Creating Any New
Abstraction"](../../CLAUDE.md#before-creating-any-new-abstraction)).
Decode-output texture rings were the *unspoken* shape across
H.264 / H.265 / `bgra_file_source` / future JPEG / camera —
every consumer hand-rolled the same pattern (or worse, the
decoders escalated every frame and allocated a fresh
`VkImage`). Lifting the shape into the engine:

- **Eliminates per-frame `escalate()`.** Escalation acquires
  `processor_setup_lock` and `vkDeviceWaitIdle()`s the device
  after — that's a full GPU sync on the per-frame critical path.
  The AGP drone-racing vision pipeline (`docs/architecture/`-
  adjacent VADR-TS-002, 30 Hz JPEG-over-UDP, latency-critical
  control loop) cannot afford that stall. See the [Honest
  accounting](#honest-accounting-of-whats-eliminated-vs-residual)
  section above for what specifically goes away and what residual
  per-frame cost remains in `upload_buffer_to_image`.
- **Eliminates per-frame `vkAllocateMemory`.** On NVIDIA Linux,
  the per-process DMA-BUF cap means continuous per-frame
  allocation pressure can suddenly start returning fake-OOM.
  `create_texture_local` already skips the DMA-BUF pool, but
  the cap-pressure goes deeper than DMA-BUF and pre-allocation
  is the durable fix.
- **Makes the right way easy and the wrong way hard.**
  `copy_pixel_buffer_to_texture` is Limited-safe and one method
  call; `upload_pixel_buffer_as_texture` (the old per-frame
  escalation point) stays FullAccess-only as a deliberate
  bumper — consumers who try to take the old path get a
  capability-tier mismatch at compile time.

The capability-split moat ([CLAUDE.md "Enforce Capability
Split"](../../CLAUDE.md)) is preserved: privileged work
(allocation, descriptor-set construction, pipeline creation)
stays on FullAccess; the new Limited primitive does only what
existing Limited methods already do (write to a pool buffer's
mapped memory, sample a registered texture, submit to the
shared queue). No new attack surface — the queue submit is
serialized by the existing queue mutex, and a Limited caller
can only invoke `copy_pixel_buffer_to_texture` against a
texture they already own (which they could only have gotten
via FullAccess somewhere upstream — typically via a ring
constructed in `setup()`).

## Reference

- **Implementation**:
  - `runtime/streamlib-engine/src/core/context/texture_ring.rs`
  - `GpuContextFullAccess::create_texture_ring`,
    `GpuContextLimitedAccess::copy_pixel_buffer_to_texture` in
    `runtime/streamlib-engine/src/core/context/gpu_context.rs`
- **First consumers (CPU-upload shape)**:
  - `packages/h264/src/linux/decoder.rs`
  - `packages/h265/src/linux/decoder.rs`
  - `packages/debug-utilities/src/bgra_file_source.rs`
- **Related abstractions**:
  - [`texture-registration.md`](texture-registration.md) —
    per-surface lifecycle record the ring populates at
    construction
  - [`compute-kernel.md`](compute-kernel.md) — companion
    "single canonical abstraction" pattern for compute work
  - `docs/learnings/vulkan-frames-in-flight.md` — sizing
    rationale for `MAX_FRAMES_IN_FLIGHT = 2`
  - `docs/learnings/nvidia-dma-buf-after-swapchain.md` — the
    NVIDIA-cap pressure pre-allocation avoids
