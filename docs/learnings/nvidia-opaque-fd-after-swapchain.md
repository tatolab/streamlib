# NVIDIA Linux: OPAQUE_FD allocations are capped after swapchain creation

## Symptom

`VK_ERROR_OUT_OF_DEVICE_MEMORY` returned from `vmaCreateBuffer` (or
the host RHI's `HostVulkanBuffer::new_opaque_fd_export*`) when:

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
`HostVulkanBuffer::new_opaque_fd_export_device_local` after the
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

The exact mechanism is internal to NVIDIA's driver. What's
empirically observable: a successful `vkAllocateMemory` for the
handle type *before* `vkCreateSwapchainKHR` keeps the post-swapchain
allocation path open only while a live allocation of that handle type
anchors the per-handle-type state. For DMA-BUF that always holds: the
compositor's swapchain DMA-BUF imports keep a live DMA-BUF allocation
in the kernel for the process's lifetime, so the state never observes
"no live consumer" and a freed probe is enough. For OPAQUE_FD nothing
equivalent exists between the engine pre-warm and the consumer's
request, so the state is reclaimable once the probe is freed. On Cam
Link 4K specifically, the slower MMAP+memcpy camera startup gave the
kernel enough time for the state to decay, and the consumer's
post-swapchain OPAQUE_FD allocation flaked intermittently.

This is **not** about VMA block retention. The host RHI's pixel-
buffer and texture constructors all set
`vma::AllocationCreateFlags::DEDICATED_MEMORY`, so each export
allocation is its own `VkDeviceMemory`; the *DMA-BUF* probe still
issues a real `vkFreeMemory` and what survives is on the
compositor's side (live swapchain DMA-BUF imports), not on VMA's.
For OPAQUE_FD the engine keeps its probe alive permanently,
since neither the compositor nor any other ambient consumer
provides a live OPAQUE_FD allocation to anchor the kernel state.

## Fix

`HostVulkanDevice::new()` pre-warms every export-capable VMA pool —
DMA-BUF buffer, DMA-BUF image linear, DMA-BUF image tiled, OPAQUE_FD
HOST_VISIBLE buffer, OPAQUE_FD host-cached buffer, OPAQUE_FD
DEVICE_LOCAL buffer, OPAQUE_FD image — strictly before any caller can
build a `VkSwapchainKHR`.

DMA-BUF probes are **allocate-and-drop** through the standard host
RHI constructors. This works because the compositor's
swapchain DMA-BUF imports provide a continuous live consumer for
the DMA-BUF kernel state.

OPAQUE_FD probes are **retained as long-lived sentinels** on the
device (`HostVulkanDevice::opaque_fd_export_sentinels`). All four
sentinels (HOST_VISIBLE buffer, host-cached buffer, DEVICE_LOCAL
buffer, image) are intentionally **tiny** (8×8×4 = 256 bytes; the
image one allocates an `R8G8B8A8_UNORM` `VkImage` with the same byte
budget): empirical E2E on Cam Link 4K (run during PR
`fix/opaque-fd-export-sentinels-637`) showed a consumer-resolution
buffer sentinel (1920×1080×4 ≈ 8 MiB) *deterministically* blocked the
consumer's same-size post-swapchain allocation, indicating NVIDIA
tracks a cumulative byte budget on top of the per-handle-type state.
Sentinels exist only to pin the per-handle-type kernel state, so they
must not compete with consumer-class allocations. Each sentinel frees
its own allocation on drop, and must drop before its pool: VMA's
`vmaDestroyPool` over a live dedicated allocation aborts with
`Unfreed dedicated allocations found!` — the same assertion text as an
allocator teardown (#2247). `HostVulkanDevice::Drop` clears the
sentinels before destroying any pool, and a pre-warm that returns early
frees the sentinels it already made.

**Image-flavored sentinel — provisional retention pending consumer.**
The OPAQUE_FD image pool ships a matching retained sentinel that
uses the same "tiny, per-handle-type" shape as the buffer sentinels.
To the best of our current knowledge the empirical verification
protocol (sections A/B/C below) cannot be run for the image sentinel
today: `HostVulkanTexture::new_opaque_fd_export` is the engine
primitive but no in-tree consumer of OPAQUE_FD `VkImage`s
post-swapchain exists yet — the reproducers below allocate OPAQUE_FD
*buffers*, not images. The image sentinel is retained out of
conservatism: the cap mechanism
is spec-level "per-handle-type kernel state", the buffer-side
evidence makes the same argument for the image side, and a tiny
sentinel costs ~256 bytes for the device's lifetime. When a real
consumer-class OPAQUE_FD `VkImage` allocator lands in-tree, the
sentinels-dropped protocol (Section C below, adapted for images)
becomes runnable and the retention should be re-validated. If the
empirical run shows the image sentinel is redundant given the
already-retained buffer sentinels, dropping it is a one-line change
in `prewarm_export_pools`. If it's load-bearing, the existing
data-structure-level test
(`opaque_fd_export_sentinels_retained_for_each_supported_pool`,
which already includes `opaque_fd_image` in its expected-labels
list when the pool is constructed) becomes the regression lock.

**Failed cross-device DMA-BUF import probes are skipped on NVIDIA.**
A failed `vkAllocateMemory` chained with `VkImportMemoryFdInfoKHR` is
NOT side-effect-free on NVIDIA, even when the call returns cleanly —
per-handle-type kernel accounting carries forward. The camera's failed
cross-device DMA-BUF import probe perturbs NVIDIA's OPAQUE_FD
allocation accounting despite the engine sentinel: with the small
sentinels alone the Cam Link 4K cold-shell pass rate was 9/10, and
force-skipping the probe on NVIDIA moved it to 10/10 without touching
anything else. `HostVulkanDevice::supports_cross_device_dma_buf_probe()`
returns `false` when `vendor_id == 0x10DE` (NVIDIA), and the V4L2
capture backend gates its DMA-BUF probe on it. Mesa drivers (Intel
iris, AMD radeonsi) tolerate the failed probe, so it runs there. To the
best of our current knowledge no driver release notes have published
this. If a future NVIDIA driver release fixes it, the blocklist is a
one-line update in `vulkan_device.rs::HostVulkanDevice::new()`.

`new()` returns `Result<Arc<Self>>` so the pre-warm step can call
back through the public RHI constructors (which take
`&Arc<HostVulkanDevice>`). This is the production pattern (Unreal
`RHICreateDevice`, Bevy renderer init, wgpu-core `request_device`):
construction either yields a fully-usable instance with all
init-time invariants run, or fails — there is no half-formed state
observable to callers. Sentinel storage is bypassed in the wrapper
chain (raw `vk::Buffer` / `vk::Image` + `vma::Allocation` + the
device's `Arc<vma::Allocator>`, not `HostVulkanBuffer`) to avoid the `Arc<HostVulkanDevice>`
back-reference cycle that would prevent the device from ever
dropping.

**Consumers do NOT need to pre-warm.** If you find yourself wanting
to allocate-and-drop an exportable resource at processor `start()`
time, don't: the engine already did it before any of your code ran.
See CLAUDE.md's "Engine-wide defects get fixed at the engine layer" rule.

## Verifying / re-deriving the fix

Each protocol reverts one fix, runs a pipeline in which a consumer
allocates an OPAQUE_FD buffer
(`HostVulkanBuffer::new_opaque_fd_export_device_local`) after the
display's swapchain exists, then restores the fix and re-runs. The
failure is that allocation returning `A device memory allocation has
failed`. A healthy pre-warm logs `HostVulkanDevice export pool
sentinel retained: opaque_fd_device_local (256 bytes)` plus
`HostVulkanDevice export pools pre-warmed`.

### A. Pre-warm-removed protocol (catches the original #624 bug)

Comment out the `prewarm_export_pools` call in
`HostVulkanDevice::new()`. The consumer's post-swapchain allocation
fails deterministically on vivid (`/dev/video2`). With the pre-warm
re-enabled, it succeeds.

### B. Probe-gate-removed protocol (catches the #638 regression)

With the engine sentinels intact, remove the
`supports_cross_device_dma_buf_probe` gate in the V4L2 capture backend
(or change the `HostVulkanDevice::supports_cross_device_dma_buf_probe()`
body to always return `true`). On Cam Link 4K (`/dev/video0`), at
least one of 10 cold-shell runs fails (1/10 observed in PR
`fix/opaque-fd-export-sentinels-637`'s E2E and the #638 retest). With
the gate restored, 10 runs give zero failures.

Vivid (`/dev/video2`) does NOT reproduce because the
`is_virtual_device` check skips the probe regardless. Only real UVC
hardware exercises the failed-probe path that perturbs OPAQUE_FD
accounting.

### C. Sentinels-dropped protocol (catches the #637 regression)

Make `prewarm_export_pools` return `Vec::new()` instead of pushing
OPAQUE_FD sentinels, OR change the `Drop` impl to take and free the
sentinels *before* any consumer can allocate (defeating the
long-lived purpose). Keep the rest of the pre-warm intact (DMA-BUF
probes still allocate-and-drop). On Cam Link 4K (`/dev/video0`) the
first cold-shell run fails intermittently, not deterministically:
10× repeats are needed; expect 1–3 failures in a fresh run. Vivid
does NOT reproduce. With the sentinels restored, 10 runs give zero
failures.

If a fresh driver stops reproducing these failures, the NVIDIA-side
mechanism may have changed and the size-class / decay model in this
learning is stale. Update accordingly.

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
- Bug fix #3 (probe-gate via vendor-id capability query): issue #638,
  same PR `fix/opaque-fd-export-sentinels-637`. The 1/10 residual
  after fix #2 was caused by the camera processor's failed
  cross-device DMA-BUF import probe perturbing OPAQUE_FD accounting
  on NVIDIA. Engine-layer fix:
  `HostVulkanDevice::supports_cross_device_dma_buf_probe()`,
  `false` when `vendor_id == 0x10DE`. The probe-gate-removed protocol
  in section B above is the reproducer.
- Sibling learning: @docs/learnings/nvidia-dma-buf-after-swapchain.md
- VMA pool pattern: @docs/learnings/vma-export-pools.md
- Empirical verification protocol above documents the reproducer
  (since the bug doesn't trigger in isolated unit tests, per the
  DMA-BUF learning). The data-structure-level invariant is locked
  by `vulkan_device::tests::opaque_fd_export_sentinels_retained_for_each_supported_pool`.
