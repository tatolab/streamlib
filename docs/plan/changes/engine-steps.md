# engine-steps

Short, portable steps for GPU work from Python. This delta builds the four
`[engine-steps-for-effects-and-model-input]` entries the owner decided 2026-09-22 (§Graphics
`ARCHITECTURE.md:1180-1213`, §Product `:45-52`; ADR
`docs/decisions/engine-steps-for-effects-and-model-input.md`; design memo
`docs/research/2026-09-22-engine-steps-for-effects-and-model-input.md`):
- the tensor buffer Python can hold;
- `GlslPixelEffect` and `ModelInputTensorKernel` over it and over the shipped primitives;
- the two-processor scaffold.

It rides milestone 52, *Apple Silicon at Linux parity*, by the owner's timing choice. It is
built Linux-first, and each macOS arm rides a parity ticket and duplicates none of them.

**Scale gate — this skill, plus an ADR (landed in #2425).** It adds a Python-public method and
two wheel classes, a new surface kind on the IPC wire, and buffer bindings through the escalate
dispatch.

**Precondition.** Every entry this delta touches is DECIDED:
- the four entries above;
- the surface-id lifetime contract (`:251-283`);
- the kernel-kind parity entry, now naming storage buffers decided (`:1004-1020`);
- "importability is an allocation flavour the engine derives per acquisition, never a Python
  dial" (`:1028`);
- "dispatch is synchronous" (`:1166`);
- the parity delta's importer ruling: the consumer RHI on both floors, with Metal only as
  exported handles, "the `MTLBuffer` behind an imported buffer for the tensor capsule"
  (`macos-capability-parity.md:101-108`).

`portable-gpu-interop` (#2420, #2421) and `macos-capability-parity` stay active beside it.

**Verified against the tree, 2026-09-22** (two read-only recon passes: IPC and RHI).

- **The allocation exists.**
  - `GpuContext::create_opaque_fd_export_buffer(byte_size, device_local=true)`
    (`gpu_context.rs:2465-2487`) returns a `StorageBuffer` over `new_opaque_fd_export_device_local`
    (`vulkan_buffer.rs:682-750`, STORAGE|TRANSFER usage, DEDICATED_MEMORY).
    `export_storage_buffer_opaque_fd` (`gpu_context.rs:2501`) yields (fd, size, device UUID).
  - The existing `acquire_storage_buffer(byte_size)` (`:1773-1799`) is HOST_VISIBLE and DMA-BUF on
    Linux. CUDA cannot import DMA-BUF, and one NVIDIA allocation cannot export both flavours
    (`surface_store.rs:719-722`). On macOS it has no export at all. Its tiers split: Full is
    Linux-only (`:3790`); Limited and base are linux|macos (`:4072`, `:1779`).
- **CUDA reads a flat OPAQUE_FD buffer with no staging.** `import_opaque_fd_into_cuda`
  (`python_cuda_pixel_exchange.rs:205-260`) uses `cudaExternalMemoryGetMappedBuffer`, and the
  device-export staging is already such a buffer. Two things to verify rather than assume:
  - cudarc passes `flags: 0`, not `cudaExternalMemoryDedicated`;
  - the size handed to CUDA is the buffer's byte size, not the allocation size.
  4,915,200 B and 2,457,600 B are page multiples; other shapes may not be.
- **NVIDIA fake OOM.** A DEVICE_LOCAL OPAQUE_FD allocation made after a swapchain exists can fail
  with a fake out-of-memory. It is mitigated by a sentinel (`docs/learnings/nvidia-opaque-fd-after-swapchain.md`).
  A tensor buffer acquired in `setup()` with the display up is exactly that case.
- **macOS route, already built for pool slots.**
  - `create_private_iosurface_with_packed_rows` (`apple/iosurface.rs:72`, never global).
  - `HostVulkanBuffer::from_iosurface_pages` (`vulkan_imported_iosurface_storage_buffer.rs:126-195`,
    `VK_EXT_external_memory_host`, 16 KiB alignment handled, refuses below MoltenVK 1.4.1).
  - `export_iosurface_mach_send_right` (`:219`).
  - MoltenVK backs host-imported memory with `newBufferWithBytesNoCopy` (MVKDeviceMemory.mm:240,
    :416), and `vkExportMetalObjectsEXT` returns that same MTLBuffer (MVKDevice.mm:5356), so the
    `kDLMetal` capsule aliases the IOSurface pages.
  - `VK_EXT_metal_objects` is enabled (`vulkan_device.rs:939-946`); nothing calls the export yet (#2404).
  - Unmeasured: IOSurface maximum width and row-pitch alignment for a byte-shaped surface.
- **Rust already binds a storage buffer.**
  - `VulkanComputeKernel::set_storage_buffer_storage` (`vulkan_compute_kernel.rs:1059`, offset
    alignment checked `:377-384`).
  - The wire already spells `storage_buffer` (`escalate_request.rs:455-479`, `:563`, `:1207`), and
    so does the helper (`python_processor_context.rs:2440-2468`).
  - Python dispatch refuses it twice. The planner rejects it by name
    (`subprocess_escalate.rs:2133-2139`, and graphics at `:2600`). The engine binding value is
    texture-only (`BatchedComputeKernelDispatchBinding`, `gpu_context.rs:752-781`), and the batch
    recorder tracks image barriers only (`:3311+`).
  - Escalate handles are per-helper (`subprocess_escalate.rs:207-218`), so a downstream helper
    cannot bind a producer helper's id without a parent-wide map. The sibling is `texture_cache`
    (`gpu_context.rs:821`).
- **The surface-share service is nearly shape-agnostic.** `resource_type` is a free string
  (`surface_store.rs:148-152`) and width/height/format are opaque. The consumer side is
  pixel-shaped: `import_checked_out_surface` refuses unknown types and derives a `PixelFormat`
  (`python_helper_process_pixel_exchange.rs:2467-2503`). `PixelExchangeTensorLayout {shape, strides,
  dtype}` is general, and only its constructors are pixel-derived (`python_gpu_surface_pixel_exchange.rs:234-300`).
- **Lifetime machinery.**
  - Reusable: the lease registry (`surface_check_out_lease_registry.rs`, counted checkouts,
    per-connection reclaim, `is_checked_out_by_any_holder` `:289`), the generation grammar
    (`<slot>#<gen>`, `pixel_buffer_pool.rs:61-122`, `publish_frame_generation` `:186`), and
    `claim_surface_against_producer_reuse`.
  - Gaps: acquired ids are bare UUIDs that never retire, and `ProcessorOutputTextureRing` rotates
    unconditionally (`processor_output_texture_ring.py:93-108`) — it never skips a held slot.
- **Downstream reading.** A bag moves a `surface_id` string and nothing more. `VideoFrame` requires
  width/height (`video_frame.py:57`), and `PythonGpuSurfaceHandle` exposes width/height/format
  (`python_processor_context.rs:111-131`). A tensor surface needs a `resolve_surface` arm and a
  handle that states its shape.
- **Fan-out.** One output feeds up to 32 inputs (`streamlib-ipc-types/src/lib.rs:341`), and the
  fisheye app already fans out (`examples/fisheye-object-detection/app.py:86-96`).

---

## §Graphics — RHI / GPU

- ADDED: a tensor surface kind on the wire and in the surface store: `resource_type`
  `storage_buffer`, carrying `shape` and `dtype` beside the memory handle. It is never a pixel
  buffer in disguise (the ADR rejects a float pixel buffer standing in for a tensor). Registration
  carries `exporting_device_uuid` and `vk_memory_type_index`, as the OPAQUE_FD texture payload
  does (`surface_store.rs:855-912`). Its lookup reply echoes shape and dtype.
- MODIFIED: `acquire_storage_buffer` takes `(shape, dtype)` on the Python surface. The engine
  derives the allocation flavour per acquisition, as it does for textures (`:1028`):
  - a buffer that crosses to a helper or to CUDA takes DEVICE_LOCAL OPAQUE_FD on Linux and a
    private byte-shaped IOSurface on macOS;
  - a caller-held Rust buffer keeps HOST_VISIBLE.

  One method, not a parallel `acquire_tensor_buffer`. Rust callers of the byte form move to it.
  Its tier split closes with #2403's un-gating: the Full arm is Linux-only today.
- ADDED: the escalate op `acquire_storage_buffer {request_id, shape, dtype}`.
  - **Reply:** `EscalateResponseOk` gains optional `shape` and `dtype` (`deny_unknown_fields`
    requires it).
  - **Handles:** a `RegisteredHandle::StorageBuffer` variant, released through the existing
    release path (`subprocess_escalate.rs:4369`).
  - **Tests:** wire vectors.
- ADDED: a parent-wide surface id → `StorageBuffer` map beside `texture_cache`, so any helper binds
  any helper's tensor surface by id. The retired-generation gate (`refuse_a_retired_frame_id`)
  covers the buffer path.
- MODIFIED: dispatch binds `storage_buffer` by surface id, for compute and graphics:
  - the planner stops refusing it;
  - `BatchedComputeKernelDispatchBinding` holds texture or buffer;
  - `write_into_kernel` calls `set_storage_buffer_storage`;
  - the batch recorder adds buffer memory barriers between passes.

  Uniform buffers stay refused; ray tracing's buffer refusal (`escalate_request.rs:1796`) is
  unchanged. Dispatch stays synchronous.
- ADDED: DLPack export by declared shape.
  - **Constructor:** `PixelExchangeTensorLayout` gains a `(shape, dtype)` constructor with
    contiguous element strides.
  - **Linux:** a tensor surface is checked out once and CUDA-imported directly as linear memory
    (`kDLCUDA`) — no staging, no refill, no copy-back. A CUDA-side write is ordered back to Vulkan
    by the device-wide synchronize the write-back path already uses.
  - **macOS:** the capsule is `kDLMetal` over the MTLBuffer exported from the helper's imported
    buffer. It rides #2402 (Mach sidecars, consumer-RHI IOSurface arm) and #2404
    (`vkExportMetalObjectsEXT` → `kDLMetal`). Its tests carry
    `awaiting_macos_parity(issue=2404)`.
- ADDED: a tensor surface resolves downstream. `resolve_surface` imports the new `resource_type`
  and yields a handle that states `shape` and `dtype`. Width, height and format are not invented
  for it. Its bare `__dlpack__` is the read path. No typed cast object for tensors in this change —
  a consumer reads by id; the cast-object claim seam is shape-agnostic and composes later.
- ADDED: `GlslPixelEffect` in the wheel's Python, over `create_compute_kernel`,
  `ProcessorOutputTextureRing`, `copy_surface_to_surface` (#2420) and dispatch. No engine and no
  wire change. The spelling:

  ```python
  def setup(self, ctx: RuntimeContextFullAccess) -> None:
      self.invert = GlslPixelEffect.compile(
          ctx.gpu_full_access,
          effect_glsl="vec4 effect(vec4 source, ivec2 at) { return vec4(1.0 - source.rgb, source.a); }",
          dials={"strength": "float"},
      )

  def process(self, ctx: RuntimeContextLimitedAccess) -> None:
      frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
      if frame is not None:
          ctx.outputs.write("video_to_downstream",
              self.invert.apply_to_frame(ctx.gpu_limited_access, frame, dials={"strength": 1.0}))
  ```

  One single-plane RGBA source, output at its extent in `rgba8_unorm`.
  - **Dials:** push constants typed `float`, `int`, `vec2` or `vec4`, with `vec3` refused. The
    push block must fit 128 bytes, and larger is refused.
  - **Pre-declared:** `streamlib_extent`, `streamlib_elapsed_seconds` (monotonic), and the
    `streamlib_source_at` / `streamlib_source_uv` sampling helpers.
  - **Diagnostics:** `#line 1` before the user's body, so shaderc names the user's own line.
  - **Named refusals:** a missing `effect` signature, an undeclared or missing dial, a `vec3` dial,
    an oversize push block, and a frame the copy refuses.

  Blocked by #2420, whose `rgba` vs `rgba8_unorm` question it inherits.
- ADDED: `ModelInputTensorKernel` in the wheel's Python: one compute pass from an RGBA surface into
  a tensor surface from the output ring decision 1 settles.
  - **Parameters:** target size; fit `stretch`, `letterbox` or `pad_bottom_right` with
    `pad_to_multiple_of`; channel order with alpha dropped; layout `nchw` or `nhwc`; `float32`
    or `float16`; `scale`, `mean`, `std`.
  - **Returns:** `apply_to_surface(gpu_limited_access, surface)` returns
    `(tensor_surface, geometry)`, where `geometry.boxes_to_source(...)` maps detections back to
    source coordinates.
  - No colour conversion.

## §Product — the MVP sentence

- MODIFIED: `scaffold_new_app` (`cli.py:413-454`) writes the two-processor app once both floors run
  it:
  - an `InvertingEffect` over `GlslPixelEffect` in the camera→window path;
  - a numpy `BrightnessMeter` on a fan-out of the effect's output, which reads a strided
    `frame.cpu()` view and logs the mean once a second on the monotonic clock.

  `pyproject` keeps `["streamlib", "numpy>=2.1"]`. The flip waits on #2420, #2403 and
  `GlslPixelEffect`, green on both lanes. The same PR rewrites the scaffold's write-combined
  comment #2361 already makes true on both floors. The existing scaffold tests (`test_cli.py:435-541`,
  `test_cli_launch.py:450`) and the cross-floor check's scaffold gate (#2421) hold it.

## §Consumers — examples

- MODIFIED: `fisheye-object-detection`'s `_detections_in` pre-processing
  (`undistorting_object_detector.py:275-300`) becomes a `ModelInputTensorKernel` with
  `fit="pad_bottom_right", pad_to_multiple_of=32`, preserving its "boxes already in frame
  coordinates" property. It lands after #2423 (the device literal) and never blocks an engine
  ticket.

## Removals

- REMOVED: surface-backed kinds are storage_image and sampled_texture
  The dispatch planners' refusal of `storage_buffer` (`subprocess_escalate.rs:2137`, `:2600`).
  Replaced by a refusal that names uniform buffers only.

---

## [NEEDS DECISION] 1 — Where a tensor ring lives

The surface-id lifetime contract (DECIDED, `:251-283`) says a published surface is immutable
while held: "the pool skips leased slots and grows to its cap; at cap the producer drops its own
frame". The plan's tensor-buffer entry applies that to tensor rings. `ModelInputTensorKernel`
publishes a new tensor every frame, so it needs a ring that honours it. The tree has two shapes
and neither does it for anything but pixel buffers: the pixel-buffer pool's lease-aware acquire,
and a wheel ring that rotates blindly. (`ProcessorOutputTextureRing` has the same gap for
textures today — see Notes.)

**A — An engine-owned pool for tensor surfaces (recommended).** `acquire_storage_buffer` takes an
optional pool key, and the engine's lease-aware acquire hands back a free slot:
- it mints `<slot>#<gen>` per publication with `publish_frame_generation`;
- it skips leased slots and grows to a cap;
- at the cap it refuses by name, so the producer drops its own frame.

This extends the one lifetime system that exists. The wheel ring asks the engine for the next
slot instead of rotating. Cost: the pool generalises past pixel buffers.

**B — A wheel ring plus a "still held?" escalate op.** The helper asks per rotation and skips held
slots itself. Cost: a second place that decides slot reuse, per-frame round trips, and generation
ids minted outside the pool — two lifetime systems.

**C — No ring: one tensor buffer per processor.** It is rewritten only after the downstream
consumer releases it, which is safe only when the consumer is the producing processor itself.
Cost: it cannot publish downstream, which the DECIDED entry allows.

Recommendation: **A.** It is the existing system extended rather than a parallel one.

## Not in scope

- a typed cast object for tensors;
- uniform-buffer bindings;
- a `cpu()` door on a tensor surface — torch's `.cpu()` covers it;
- multi-input pixel effects;
- a Rust `GlslPixelEffect`;
- folding the copy into the dispatch;
- #516's forward-forward layer itself, which consumes this capability later.

## Notes

- `ProcessorOutputTextureRing` rotates without checking leases, so a downstream holder of an
  earlier output can see it rewritten. That contradicts the surface-id lifetime contract for
  kernel outputs today. It is outside this delta's scope unless decision 1 is A and the owner
  wants the texture ring moved onto the same engine pool in the same change.
