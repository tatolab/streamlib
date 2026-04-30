# Host-side texture readback in the RHI

streamlib's RHI exposes one canonical primitive for reading a host
`StreamTexture` back into CPU memory: `VulkanTextureReadback` (Linux)
plus the public binding-shape types in `core::rhi`
(`TextureReadbackDescriptor`, `TextureSourceLayout`, `ReadbackTicket`,
`TextureReadbackError`). **Every callsite that needs to copy GPU pixels
back to the host uses this abstraction — do not hand-roll a staging
buffer, command pool, command buffer, fence, or queue submit for an
image-to-buffer copy.**

This is engine-model territory: the RHI is the single gateway to the
GPU, and the readback handle is the single gateway for image →
HOST_VISIBLE buffer transfer. Adding a third shape every time a new
binary needs to grab pixels is the failure mode this doc exists to
prevent.

## What the abstraction does for you

Given a fixed format/extent declared at construction, the handle:

1. **Allocates the staging buffer once** through VMA's default
   allocator (HOST_VISIBLE | HOST_COHERENT, persistent-mapped). No
   per-submit allocation. Sized to `width * height * bpp`.
2. **Owns its command pool, command buffer, and timeline semaphore.**
   None of this is your code anymore.
3. **Records `vkCmdCopyImageToBuffer` with full barrier semantics**
   per submit — pre-barrier `source_layout → TRANSFER_SRC_OPTIMAL`,
   the copy, post-barrier `TRANSFER_SRC_OPTIMAL → source_layout`
   (so the texture is restored to its prior layout) and a buffer
   barrier `TRANSFER_WRITE → HOST_READ` so the host map sees
   coherent bytes.
4. **Submits through `HostVulkanDevice::submit_to_queue`** — same
   per-queue mutex everything else in the engine uses.
5. **Signals a timeline semaphore at a monotonic counter value.**
   The returned `ReadbackTicket` wraps that counter; callers wait
   on the timeline via `try_read` / `wait_and_read`.

## Adding a new readback callsite — the recipe

1. **Construct a handle once at setup time** for each
   `(format, extent)` you need to read back:

   ```rust
   let readback = gpu.create_texture_readback(&TextureReadbackDescriptor {
       label: "my-binary/readback",
       format: TextureFormat::Bgra8Unorm,
       width: 512,
       height: 512,
   })?;
   ```

   Store the `Arc<VulkanTextureReadback>` somewhere it outlives the
   read loop (a slot owned by `main`, a struct field on a processor,
   etc.).

2. **Submit + wait per frame:**

   ```rust
   let ticket = readback.submit(&texture, TextureSourceLayout::General)?;
   let bytes = readback.wait_and_read(ticket, u64::MAX)?;  // &[u8]
   // ... consume `bytes` ...
   ```

   `bytes` borrows the handle's mapped staging buffer; the next
   `submit` is a compile-time conflict with that borrow, which is
   exactly the safety invariant we want (the next GPU copy would
   overwrite the bytes you're holding).

3. **Zero-copy variant** for streaming bytes into a sink (ffmpeg
   stdin, PNG encoder, etc.):

   ```rust
   readback.wait_and_read_with(ticket, u64::MAX, |bgra| {
       ffmpeg_stdin.write_all(bgra)
   })??;
   ```

   The closure runs after the wait completes; the handle resets to
   idle when it returns.

## Single-in-flight per handle

Each handle has one staging buffer + one command buffer + one
timeline semaphore. A second `submit()` before the prior ticket is
waited returns `TextureReadbackError::InFlight`. For genuine parallel
readbacks, **hold N handles** — one per in-flight slot. Mirrors
`VulkanComputeKernel`'s shape exactly.

The trade-off is intentional: the alternative (ringed staging buffers
inside one handle) doubles the memory cost per handle and complicates
the borrow story for the returned `&[u8]`. Real callers either need
sequential reads (every frame, drained before the next) or a small
fixed parallelism (N ≤ frames-in-flight) — both are clean as N
handles.

## What's deliberately not covered

- **Multi-plane formats** (NV12 etc.). Single-plane only today —
  every shipped polyglot example reads a packed BGRA / RGBA surface.
  Extend the abstraction with a `plane_count` knob when the first
  multi-plane host-side readback callsite arrives.
- **Caller-supplied staging buffers.** The handle owns its staging
  buffer. If a future caller has reason to read into someone else's
  buffer (e.g. into a pre-mapped `RhiPixelBuffer`), extend the
  abstraction with an `into_buffer` variant.
- **Asynchronous-with-callback API** (`submit(then: impl FnOnce)`).
  The current `try_read` / `wait_and_read` shape is sufficient for
  every shipped callsite. Add a callback variant if a future caller
  needs it.
- **Cross-queue readback** (transfer queue instead of graphics).
  `submit_to_queue` automatically picks the right per-queue mutex
  based on the queue handle, but the readback today always uses the
  device's primary graphics queue. A `with_queue` knob is the right
  extension point if a future caller wants the dedicated transfer
  queue for compute-graphics overlap.
- **Allocation-failure simulation** in unit tests. VMA out-of-budget
  is reachable in adversarial tests; we don't have a reliable
  in-process knob to trigger it deterministically. The error
  taxonomy still names `StagingBufferAlloc`; coverage is via real
  allocation pressure in E2E.

## Why this shape

Production realtime engines (Unreal `FRHIGPUTextureReadback`, bgfx
`bgfx::readTexture`, WebGPU `copyTextureToBuffer + mapAsync`,
Granite's timeline-semaphore-backed copy) all converge on the same
answer: a pre-allocated staging buffer + fenced or timeline-wait
ticket. We picked the timeline-semaphore variant because:

- Timeline semaphores are already the codebase's preferred sync
  primitive (see `vulkan_command_buffer.rs`,
  `streamlib-adapter-cpu-readback`'s submit path,
  `consumer_vulkan_sync.rs`).
- Vulkan 1.2 core, supported on every driver streamlib targets.
- Monotonic counter values are a natural fit for ticketing —
  `try_read` is `vkGetSemaphoreCounterValue >= ticket.counter`,
  no separate fence-pool bookkeeping needed.
- Multiple in-flight reads (when the caller holds N handles) fall
  out for free — each handle's timeline is independent.

A binary fence per handle would also have worked. Timeline matches
codebase precedent and Granite's reference shape, so that's the call.

## Boundary check

Polyglot example/scenario binaries no longer carry an allowlist
entry for raw `vulkanalia` — every host-side per-frame readback
rides this primitive. If a future example needs a different shape of
readback (multi-plane, async-with-callback, etc.), extend
`VulkanTextureReadback` instead of reaching for raw vulkanalia. The
boundary check (`cargo xtask check-boundaries`) will fail any PR
that reintroduces raw vulkanalia in `examples/polyglot-*` — the
violation message points back here.

## Reference

- Implementation: `libs/streamlib/src/vulkan/rhi/vulkan_texture_readback.rs`
- Public types: `libs/streamlib/src/core/rhi/texture_readback.rs`
- GpuContext factory: `GpuContext::create_texture_readback`
  (`libs/streamlib/src/core/context/gpu_context.rs`)
- Engine precedent: `compute-kernel.md` and
  `libs/streamlib/src/vulkan/rhi/vulkan_compute_kernel.rs`
- Trade-off discussion: issue #583
