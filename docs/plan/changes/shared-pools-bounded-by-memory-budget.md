# shared-pools-bounded-by-memory-budget

> **Approved by the owner, 2026-10-12**, as written, with its stated assumptions.

A node's frames are limited by the node alone, and the runtime's shared pools only by the
device: its memory budget and its allocation-count limit. After this change:
- the runtime-wide texture pool and pixel-buffer pool cap no shape by count; the 16-per-shape
  texture bucket and the 64-per-shape pixel-buffer pool are gone;
- a node's pooled frames are bounded by its own output pool (16, as today), and its one-off
  acquisitions by a count kept for that node, refused by name at the same 16;
- every frame allocation is refused by name, before it allocates, past the device's reported
  memory budget or near its allocation-count limit, and the node drops that frame;
- no acquisition waits on another holder; the 1 s blocking acquire is deleted;
- the shared pools give memory back: an ended stream's entries are freed at once, and entries
  left unused by a live stream are freed after an idle window.

**Scale gate — this skill, plus an ADR.** It changes a shipped contract (the surface-id
lifetime entry's "grows to its cap", §Packages) and the node output pool entry (§Graphics),
and adds an RHI behaviour: the device's memory budget is read and enforced at allocation. The
rationale is `docs/decisions/shared-pools-bounded-by-memory-budget.md`.

**Origin.** Bug #2736, found validating many streams in one `tatolabd` (#2727, PR #2735; the
same wedge on 0.45.2 in #2730): from the 9th test-pattern stream on, every new stream's
`InvertingEffect` dropped every frame, refused with "every one of the 16 1280x720 Rgba8Unorm
textures this runtime's texture pool may hold is in use". Direction agreed with the owner
2026-10-12; two independent reviews tightened it; the owner chose per-node counting of one-off
acquisitions the same day (#2736 comments).

**Precondition.** Every entry edited is DECIDED: §Packages (SHIPPED) `ARCHITECTURE.md:418-445`
(surface-id lifetime); §Graphics (DECIDED) `:1878-1904` (node output pool). §Graphics' OPEN
entries (`:1747` layout cell, `:1958` depth/MSAA, `:1966` optional accelerators) are not touched;
the budget applies wherever a device exists, and a runtime with none (optional accelerators,
pivot step 5) has no pool to bound.

**Verified against the tree 2026-10-12 (HEAD 02bd6a214)**, two read-only sweeps. `E` =
`runtime/streamlib-engine/src`, `C` = `E/core/context`, `SE` =
`E/core/compiler/compiler_ops/subprocess_escalate`.
- **The shared caps.** One `TexturePool` per `GpuContext` (`C/texture_pool.rs`), buckets by
  size, format, usage and importability, `max_pool_size_per_bucket: 16` (`:155`), default
  `TexturePoolExhaustionPolicy::Block { timeout_ms: 1000 }` (`:136`) on a condvar
  (`acquire_blocking`, `:522`). `GrowPool` / `ReturnError` (`:129`, `:131`), `prewarm` (`:615`),
  `stats` (`:624`), `clear_unused` (`:629`) and `GpuContext::with_texture_pool_config`
  (`C/gpu_context.rs:1016`) have no callers. The pixel-buffer pool pre-allocates 4, grows by 4
  (`C/gpu_context.rs:23`, `:435`) to `POOL_MAX_BUFFER_COUNT = 64` per shape (`:26`), refused at
  `:508-511`.
- **Nothing is ever freed.** Dropping a `PooledTextureHandle` only marks its slot available
  (`C/texture_pool.rs:275-279`, `:416-426`). Pixel-buffer slots are registered with the surface
  store at allocation (`C/gpu_context.rs:354`, `:451`) and "held for the runtime's lifetime"
  (`:101-103`); no path calls `SurfaceStore::release` (`C/surface_store.rs:513`) for one.
- **Node output pools.** `PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY = 16`
  (`C/processor_output_surface_pool.rs:21`), refused as
  `Error::EverySlotInTheProcessorOutputPoolIsInUse` (`:122-135`). Per helper,
  `processor_output_pools_of_one_helper.rs`; the only drain is the bridge's drop
  (`E/core/compiler/compiler_ops/subprocess_bridge.rs:566-618`), reached at stream stop and node removal through
  `stop_the_helper_process_on_the_shutdown_ladder` → `self.bridge.take()`
  (`E/core/compiler/compiler_ops/processor_interpreter_spawn_host.rs:561`, `:609`).
- **One-off acquisitions.** With no `processor_output_pool`, `handle_acquire_texture`
  (`SE/acquisition/mod.rs:120`) allocates under `TexturePoolWaitWhenExhausted::AsThePoolIsConfigured`
  (`:52-72`) — the waiting path — and inserts into the helper's `EscalateHandleRegistry`
  (`SE/handle_lifecycle/mod.rs:119`), one per bridge, so one per node. So do
  `acquire_pixel_buffer` (`SE/acquisition/mod.rs:76-118`), Linux `acquire_image`
  (`SE/acquisition/linux.rs:104`) and `acquire_storage_buffer` (`:203`, a fresh allocation each,
  `C/gpu_context.rs:1923`). The registry keeps no count (`handle_count()`, `:207`, is test-only).
- **Native built-ins draw per frame from the shared pixel-buffer pool**, with no pool of their
  own: the camera's conversion stage
  (`C/captured_video_frame_to_pooled_rgba_conversion_stage.rs:149`) and the decoder
  (`E/vulkan/video/vulkan_video_codec_backend.rs:208`). Both receive only the GPU context; node
  identity is on `RuntimeContext::processor_id()` (`C/runtime_context.rs:229`).
- **Unpooled frame allocations:** `acquire_output_texture` (`C/gpu_context.rs:1554`, kernel
  outputs), `acquire_render_target_dma_buf_image` (`:1859`, `VirtualCameraSink`'s slots at
  `E/linux/pipewire_video_source.rs:374-379`), macOS `acquire_render_target_iosurface_image`
  (`:1883`).
- **No budget is read.** VMA is created with only `BUFFER_DEVICE_ADDRESS`, under ray tracing
  (`E/vulkan/rhi/vulkan_device.rs:1393`); `VK_EXT_memory_budget` is not in the extension list
  (`:874-886`). The vendored allocator already wraps both: `AllocatorCreateFlags::EXT_MEMORY_BUDGET`
  (`vendor/tatolab-vulkanalia-vma/src/flags.rs:30`) and `get_heap_budgets`
  (`vendor/tatolab-vulkanalia-vma/src/allocator.rs:115`). `maxMemoryAllocationCount` is read
  nowhere. `live_allocation_count` (`vulkan_device.rs:327`) counts raw `vkAllocateMemory`
  imports only, not VMA allocations.
- **No single allocation seam.** Textures funnel through the five `HostVulkanDevice::create_texture*`
  methods (`vulkan_device.rs:2855-2928`); pixel-buffer slots call `HostVulkanBuffer::new` /
  `new_iosurface_backed_pixel_buffer` directly (`E/vulkan/rhi/vulkan_pixel_buffer_pool.rs:15-40`);
  codec code calls VMA directly from `E/vulkan/video/` (e.g. `codec_utils/vk_image_resource.rs:444`).
- **Destruction is unfenced and nothing records a resource's last use.** `HostVulkanTexture::drop`
  (`vulkan_texture.rs:1669`) and `HostVulkanBuffer::drop` (`vulkan_buffer.rs:1354`) free at once;
  completion is tracked per recorder only (`vulkan_command_recorder.rs:285`, `:858`).
- **NVIDIA exportable flavours.** Four 8×8 OPAQUE_FD sentinels are held for the device's life
  (`vulkan_device.rs:1941-2028`); the DMA-BUF probes allocate and drop (`:1885-1934`), relying on
  the compositor's imports to keep the kernel state alive
  (`docs/learnings/nvidia-opaque-fd-after-swapchain.md`).
- **macOS.** Production IOSurfaces come from `create_private_iosurface` (`E/apple/iosurface.rs:260`),
  sized by `alloc_size()` (`:85`). No `MTLDevice` is held; `recommendedMaxWorkingSetSize` and
  `currentAllocatedSize` are read nowhere.

## MODIFIED: §Packages `ARCHITECTURE.md:436-439` — surface-id lifetime, the producer's bound

> … The engine inspects no bag content anywhere. The producer never waits on a consumer:
> its node's output pool skips leased slots and grows to its cap; at the cap, or past the
> runtime's memory budget (§Graphics), the producer drops its own frame — a slow consumer
> costs memory, then its own node's frames, never another processor's cadence; only the
> runtime's memory budget is shared between nodes. …

Replaces "the pool skips leased slots and grows to its cap; at cap the producer drops its own
frame — a slow consumer costs memory, then its own frames, never another processor's cadence."

## MODIFIED: §Graphics `ARCHITECTURE.md:1888-1891` — node output pool

> … It rotates through `rotation_depth` slots, grows while consumers hold frames to a cap of
> 16, and at the cap refuses by name so the producer drops its own frame; this cap is the only
> count on a node's pooled frames, the shared pools below capping no shape. A descriptor change
> replaces the pool, bridge teardown releases every slot and frees each one no holder still
> has, and a retired id is refused as recycled at resolve. …

## ADDED: §Graphics — after the node output pool entry (`:1904`)

- **DECIDED** — Frame count is per node; memory is per runtime. A node's frames are limited by
  the node alone: its pooled frames by its output pool's cap, its one-off acquisitions — a
  texture, storage buffer, pixel buffer or image taken outside that pool — by a count of 16
  held per shape, kept for the node and refused by name at that count. A native built-in's
  per-frame acquisitions draw from an output pool of its own on the same terms. The runtime's
  shared texture and pixel-buffer pools reuse allocations by shape and cap no shape by count.
  What bounds them is device memory, read per heap from the allocator's budget with
  `VK_EXT_memory_budget` enabled wherever the device advertises it: every frame allocation —
  pooled, one-off, or unpooled (a kernel output, a render target) — is refused by name before
  it allocates when the heap's reported usage plus the request would pass its budget less a
  small headroom, or when the process nears the device's `maxMemoryAllocationCount`; the check
  and the allocation are one step, so concurrent allocations never pass on one reading. A refusal
  reads a fresh budget and names the heap, its budget, its usage and the bytes the pools hold;
  the node drops that frame. Without the extension the budget is the allocator's estimate of
  80% of the heap, also capped by the system's available memory for a heap that is not
  `DEVICE_LOCAL`, and the runtime says once that its bound is an estimate. On macOS the bound is
  Metal's `recommendedMaxWorkingSetSize`, and usage is the Metal device's
  `currentAllocatedSize`, which counts the IOSurfaces the runtime mints. Memory a helper process
  allocates reaches the runtime only as a smaller budget, and a smaller budget never takes back
  a held frame. No acquisition waits on another holder. Memory is shared, so a stream that
  grows into the budget can cost another stream its frames; isolation is by count, per node.
  [shared-pools-bounded-by-memory-budget]
  Rejected: raising the shared pool's per-shape cap — any count shared across nodes lets one
  node's held frames drop another node's frames (owner, 2026-10-12).
  Rejected: bounding the shared pools only by the driver's allocation failure — NVIDIA reports
  a fake out-of-memory, and nothing would return an ended stream's memory (owner, 2026-10-12).
  <!-- verify: cargo test -p streamlib-engine one_nodes_held_frames_never_refuse_another_nodes_frames_of_the_same_shape -->
  <!-- verify: cargo test -p streamlib-engine a_frame_allocation_past_the_heap_budget_is_refused_naming_the_heap -->
  <!-- verify: cargo test -p streamlib-engine a_nodes_one_off_acquisitions_are_refused_at_its_own_count_by_name -->
  <!-- verify: cargo test -p streamlib-engine an_acquisition_at_the_budget_refuses_at_once_and_never_waits -->
- **DECIDED** — The shared pools give memory back. A pooled entry is freed once it is unheld —
  no in-process handle, no checkout lease, on macOS no kernel in-use count, and every
  submission made before its release has completed — and either its stream has stopped, its
  node's output pool was torn down, or it has sat unused past an idle window of a few seconds.
  Freeing releases the entry's surface-share registration and the parent's caches and retires
  its id in the same step. Idle entries are found on acquire and on release, a bounded number
  per call, so freeing never stalls a producer. Freeing never takes an exportable memory
  flavour to zero live allocations. An allocation an exported raw handle still names stays
  counted against the budget until that handle's holder drops it.
  [shared-pools-bounded-by-memory-budget]
  <!-- verify: cargo test -p streamlib-engine a_stopped_streams_pool_entries_are_freed_and_their_ids_retired -->
  <!-- verify: cargo test -p streamlib-engine an_entry_unused_past_the_idle_window_is_freed_and_releases_its_registration -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests every_exportable_flavour_reacquires_after_its_pool_is_freed_with_a_swapchain_live -->

## Tree consequences, by slice

**Slice 1 — the bound (#2736, rewritten to this change).**
- Enable `VK_EXT_memory_budget` where advertised (`vulkan_device.rs:874-886`, copying the
  `VK_EXT_hdr_metadata` probe at `:1065-1071`) and OR `EXT_MEMORY_BUDGET` into the VMA flags
  (`:1393`). Read `maxMemoryAllocationCount`.
- One budget-and-count check, run by every frame allocation: the five `create_texture*`
  methods, the pixel-buffer slot constructors (`vulkan_pixel_buffer_pool.rs:15-40`) and the
  storage-buffer flavours (`C/gpu_context.rs:1923`). The check and the allocation are one step
  under one device-wide lock, so two allocations on different threads cannot both pass against
  the same reading; only growth allocates, so the lock is off the per-frame path. The count is
  the `VkDeviceMemory` objects the process holds: VMA's `blockCount` (each dedicated allocation
  is its own block) plus `live_allocation_count`, never VMA's `allocationCount`, which counts
  sub-allocations.
- macOS: the working-set bound, in the RHI, weighed against the Metal device's allocated size.
- Delete the texture pool's per-shape cap, `TexturePoolExhaustionPolicy`, the condvar wait and
  the dead config surface; delete `POOL_MAX_BUFFER_COUNT`; delete
  `TexturePoolWaitWhenExhausted` (every escalate acquisition refuses at once).
- Count one-off holds per shape in `EscalateHandleRegistry`, refused by name at 16.
- Give the camera conversion stage and the decoder a node output pool each, reached with the
  node's identity.
- Tests: rewrite `at_the_cap_the_producer_is_refused_rather_than_made_to_wait`
  (`C/gpu_context.rs:6065`) against a budget the test sets; delete
  `a_full_bucket_refuses_at_once_instead_of_waiting` (`C/texture_pool.rs:713`); keep
  `every_slot_held_grows_the_pool_until_its_cap_then_refuses_by_name`. Rig: the #2736 repro —
  ten test-pattern streams in one `tatolabd`, no drops — and `heapBudget` falling while a
  helper allocates.

**Slice 2 — giving memory back.**
- Record each pooled entry's last use against queue completion, so "every submission made
  before its release has completed" can be read without a device-idle wait.
- Free at bridge teardown and node-pool teardown; find idle entries on acquire and release,
  bounded per call.
- A freed pixel-buffer slot calls `SurfaceStore::release` and leaves `buffer_cache`; a freed
  texture leaves `texture_cache` through `unregister_texture`.
- Keep one live allocation of each exportable flavour for the device's life: the OPAQUE_FD
  sentinels already do, and the DMA-BUF and tiled DMA-BUF flavours gain one each, replacing
  their allocate-and-drop probes (`vulkan_device.rs:1885-1934`), so the rule never rests on a
  compositor's imports.
- Tests: the two freeing tests above, and the rig re-acquire test for DMA-BUF, tiled DMA-BUF and
  OPAQUE_FD image pools with a swapchain live.

## Assumptions stated, not asked

- **"The same cap" reads as 16 held one-offs per node per shape**, mirroring the output pool,
  which is one shape per pool key. A node holding many differently shaped tensors is not
  refused; a node leaking one shape every frame is. Bites if a node legitimately holds more
  than 16 same-shaped one-offs at once.
- **Native built-ins get node output pools.** This follows from "a node's frames are limited by
  its own output pool" (owner, 2026-10-12): without it, a camera whose frames are held would
  grow into the whole budget. Bites in slice 1's wiring — the conversion stage and the decoder
  receive no node identity today.
- **Only frame allocations are refused by the engine.** Codec DPB and bitstream buffers,
  acceleration structures, sentinels and imports are seen through the driver's reported usage
  but not gated; a failure there stays the driver's, refused by name as today.
- **The headroom is small and fixed** (tens of MiB), not a percentage: drivers already leave
  margin inside the reported budget (ANV), so a percentage would count it twice.
- **The idle window is about 5 s.** A tuning constant inside slice 2, not a contract.

## REMOVED

- REMOVED: TexturePoolExhaustionPolicy
- REMOVED: max_pool_size_per_bucket
- REMOVED: available_condvar
- REMOVED: acquire_blocking
- REMOVED: with_texture_pool_config
- REMOVED: POOL_MAX_BUFFER_COUNT
- REMOVED: TexturePoolWaitWhenExhausted
- REMOVED: AsThePoolIsConfigured
- REMOVED: a_full_bucket_refuses_at_once_instead_of_waiting
