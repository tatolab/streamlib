# NVIDIA Linux: OPAQUE_FD allocations are capped after swapchain creation

## Symptom

`VK_ERROR_OUT_OF_DEVICE_MEMORY` returned from `vmaCreateBuffer` (or
the host RHI's `HostVulkanPixelBuffer::new_opaque_fd_export*`) when:

- Running on NVIDIA Linux Vulkan driver
- A `VkSwapchainKHR` has been created in this process
- The allocation chains `VkExportMemoryAllocateInfo::handleTypes =
  OPAQUE_FD` (the export type CUDA / OpenCL `cudaImportExternalMemory`
  expects)

Error message looks like real OOM but is NOT — the device has plenty
of free VRAM. The driver is enforcing a quota on OPAQUE_FD-exportable
memory, just as it does for DMA-BUF (see
@docs/learnings/nvidia-dma-buf-after-swapchain.md). Different export
handle type, same kernel-side budget pressure.

**Failure pattern observed in streamlib:**
`CameraToCudaCopyProcessor::setup_inner` (in
`examples/camera-python-display`) called
`HostVulkanPixelBuffer::new_opaque_fd_export_device_local` after the
display processor's render thread had created its swapchain, and the
allocation failed with `A device memory allocation has failed`. Issue
#624.

## Root cause (empirical)

NVIDIA's Linux driver tracks per-process exportable-memory state per
handle-type (DMA-BUF and OPAQUE_FD are budgeted separately, both are
affected). The Wayland/X11 compositor imports the swapchain images
as DMA-BUFs to display them — that consumes the DMA-BUF state. The
swapchain itself reserves additional kernel resources that affect
fresh `vkAllocateMemory` calls for any other exportable handle type,
including OPAQUE_FD: after `vkCreateSwapchainKHR`, the *first* call
to `vkAllocateMemory` for a handle type that hasn't been allocated
yet in this process returns `VK_ERROR_OUT_OF_DEVICE_MEMORY`.

> ~~The exact mechanism is internal to NVIDIA's driver. What's
> empirically observable: a successful `vkAllocateMemory` for the
> handle type *before* `vkCreateSwapchainKHR` is sufficient to keep
> the post-swapchain allocation path open, even after that allocation
> is freed. The reservation appears to be one-way — first allocation
> initializes some kernel-side state that survives the free.~~ —
> Superseded 2026-05-03 (issue #637, PR `fix/opaque-fd-export-sentinels-637`).
> The "drop-and-free is sufficient" claim held for DMA-BUF (the
> compositor's swapchain DMA-BUF imports keep a live DMA-BUF
> allocation in the kernel for the process's lifetime, so the
> per-handle-type state can never observe "no live consumer"). It
> did NOT hold uniformly for OPAQUE_FD: there is no compositor-
> equivalent live OPAQUE_FD allocation between the engine pre-warm
> and the consumer's request, so the per-handle-type state is
> reclaimable. On Cam Link 4K specifically, the slower MMAP+memcpy
> camera startup gave the kernel enough time for the state to
> decay, and the post-swapchain `CameraToCudaCopyProcessor::setup_inner`
> request flaked intermittently. The fix retains the OPAQUE_FD
> probe as a long-lived sentinel rather than dropping it.

This is **not** about VMA block retention. The host RHI's pixel-
buffer and texture constructors all set
`vma::AllocationCreateFlags::DEDICATED_MEMORY`, so each export
allocation is its own `VkDeviceMemory`; the *DMA-BUF* probe still
issues a real `vkFreeMemory` and what survives is on the
compositor's side (live swapchain DMA-BUF imports), not on VMA's.
For OPAQUE_FD the engine now keeps its probe alive permanently,
since neither the compositor nor any other ambient consumer
provides a live OPAQUE_FD allocation to anchor the kernel state.

## Fix

`HostVulkanDevice::new()` pre-warms every export-capable VMA pool —
DMA-BUF buffer, DMA-BUF image linear, DMA-BUF image tiled, OPAQUE_FD
HOST_VISIBLE buffer, OPAQUE_FD DEVICE_LOCAL buffer — strictly before
any caller can build a `VkSwapchainKHR`.

DMA-BUF probes are **allocate-and-drop** through the standard host
RHI constructors. This still works because the compositor's
swapchain DMA-BUF imports provide a continuous live consumer for
the DMA-BUF kernel state.

OPAQUE_FD probes are **retained as long-lived sentinels** on the
device (`HostVulkanDevice::opaque_fd_export_sentinels`). Both
sentinels are intentionally **tiny** (8×8×4 = 256 bytes): empirical
E2E on Cam Link 4K (run during PR `fix/opaque-fd-export-sentinels-637`)
showed a consumer-resolution sentinel (1920×1080×4 ≈ 8 MiB)
*deterministically* blocked the consumer's same-size post-swapchain
allocation, indicating NVIDIA tracks a cumulative byte budget on
top of the per-handle-type state. The sentinel exists only to pin
the per-handle-type kernel state, so it must not compete with
consumer-class allocations. Sentinels are freed in
`HostVulkanDevice::Drop` before the allocator is torn down.

> **Residual flake.** The small-sentinel fix improved the Cam Link 4K
> cold-shell pass rate to 9/10 in PR `fix/opaque-fd-export-sentinels-637`'s
> E2E run, but did not eliminate the flake. The 1/10 failure shape is
> identical to the original (post-swapchain `vkAllocateMemory` returns
> `VK_ERROR_OUT_OF_DEVICE_MEMORY`). Working hypothesis: the camera
> processor's failed cross-device DMA-BUF import probe (which only
> runs on Cam Link 4K — vivid/v4l2loopback skip it via the
> `is_virtual_device` gate) issues raw `vkAllocateMemory` calls with
> `VkImportMemoryFdInfoKHR{handle_type=DMA_BUF_EXT}` post-swapchain
> that may perturb NVIDIA's exportable-memory accounting in a way
> the engine-side sentinel can't cover. Tracked as a separate
> investigation (issue filed alongside #637's PR).

`new()` returns `Result<Arc<Self>>` so the pre-warm step can call
back through the public RHI constructors (which take
`&Arc<HostVulkanDevice>`). This is the production pattern (Unreal
`RHICreateDevice`, Bevy renderer init, wgpu-core `request_device`):
construction either yields a fully-usable instance with all
init-time invariants run, or fails — there is no half-formed state
observable to callers. Sentinel storage is bypassed in the wrapper
chain (raw `vk::Buffer` + `vma::Allocation`, not
`HostVulkanPixelBuffer`) to avoid the `Arc<HostVulkanDevice>`
back-reference cycle that would prevent the device from ever
dropping.

**Consumers do NOT need to pre-warm.** If you find yourself wanting
to allocate-and-drop an exportable resource at processor `start()`
time, you're re-deriving the dead pattern; the engine already did it
before any of your code ran. See CLAUDE.md's "Engine-wide bugs get
fixed at the engine layer" and "No bad patterns left behind on engine
changes" rules.

## Verifying / re-deriving the fix

Two repro shapes, depending on which behavior you want to verify:

### A. Pre-warm-removed protocol (catches the original #624 bug)

Deterministic on vivid (`/dev/video2`):

1. Edit `vulkan_device.rs` to comment out the
   `prewarm_export_pools` call inside the
   `let device = { let mut device = Arc::new(device); ... };` block
   in `HostVulkanDevice::new()`.
2. `cargo build --release -p camera-python-display`.
3. Run with `STREAMLIB_CAMERA_DEVICE=/dev/video2`,
   `STREAMLIB_DISPLAY_FRAME_LIMIT=180`, `timeout --kill-after=5 30`.
4. Without the pre-warm: log shows `Setup failed: ... CameraToCudaCopy:
   new_opaque_fd_export_device_local: ... A device memory allocation
   has failed.`
5. Re-enable, rebuild, re-run: log shows `HostVulkanDevice export
   pool sentinel retained: opaque_fd_device_local (256 bytes)`
   plus `HostVulkanDevice export pools pre-warmed`, followed by
   `CameraToCudaCopy: registered cuda OPAQUE_FD DEVICE_LOCAL
   surface_id=...` — no setup failure.

### B. Sentinels-dropped protocol (catches the #637 regression)

Reproduces the intermittent flake on Cam Link 4K (`/dev/video0`)
specifically:

1. Edit `vulkan_device.rs` so `prewarm_export_pools` returns
   `Vec::new()` instead of pushing OPAQUE_FD sentinels, OR change
   the `Drop` impl to take and free the sentinels *before* any
   consumer can allocate (defeating the long-lived purpose). Keep
   the rest of the pre-warm intact (DMA-BUF probes still
   allocate-and-drop).
2. `cargo build --release -p camera-python-display`.
3. Run with `STREAMLIB_CAMERA_DEVICE=/dev/video0` (Cam Link 4K),
   `STREAMLIB_DISPLAY_FRAME_LIMIT=180`, `timeout --kill-after=5 30`.
4. Without the sentinels: the first cold-shell run intermittently
   logs `Setup failed: ... CameraToCudaCopy:
   new_opaque_fd_export_device_local: ... A device memory allocation
   has failed.`. Vivid does NOT reproduce — only Cam Link does, and
   not deterministically. 10× repeats are needed; expect 1–3 failures
   in a fresh run.
5. Restore the sentinels, rebuild, re-run 10×: zero failures.

If step 4 stops reproducing the failure on a fresh driver, the
NVIDIA-side mechanism may have changed and the size-class /
decay model in this learning is stale. Update accordingly.

## Reference

- Bug fix #1 (drop-and-free pre-warm): issue #624, `fix(rhi): pre-warm
  export VMA pools at HostVulkanDevice construction`.
- Bug fix #2 (long-lived OPAQUE_FD sentinels): issue #637,
  `fix(rhi): retain OPAQUE_FD export-pool sentinels for the device's
  lifetime`. Surfaced as an intermittent flake on Cam Link 4K
  (`/dev/video0`) in PR #636, where the slower MMAP+memcpy camera
  startup gave the kernel time to reclaim the OPAQUE_FD per-handle-
  type state between pre-warm and the consumer's allocation. Vivid
  and v4l2loopback never reproduced because their faster startup
  beat the decay window.
- Sibling learning: @docs/learnings/nvidia-dma-buf-after-swapchain.md
- VMA pool pattern: @docs/learnings/vma-export-pools.md
- Empirical verification protocol above documents the reproducer
  (since the bug doesn't trigger in isolated unit tests, per the
  DMA-BUF learning). The data-structure-level invariant is locked
  by `vulkan_device::tests::opaque_fd_export_sentinels_retained_for_each_supported_pool`.
