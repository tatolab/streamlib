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
anchors the per-handle-type state. For OPAQUE_FD nothing outside the
engine is such an anchor, so the state is reclaimable once the last
allocation of that type is freed. On Cam Link 4K specifically, the
slower MMAP+memcpy camera startup gave the kernel enough time for the
state to decay, and the consumer's post-swapchain OPAQUE_FD allocation
flaked intermittently. The engine does not rely on the compositor's
swapchain DMA-BUF imports to anchor DMA-BUF either: the shared frame
pools free every frame of a flavour once it goes unused, so each
DMA-BUF pool holds its own anchor too.

This is **not** about VMA block retention. The host RHI's pixel-
buffer and texture constructors all set
`vma::AllocationCreateFlags::DEDICATED_MEMORY`, so each export
allocation is its own `VkDeviceMemory` and freeing it issues a real
`vkFreeMemory`. What anchors the kernel state is a live allocation of
the handle type, which is why the engine keeps one alive per export
pool.

## Fix

`HostVulkanDevice::new()` allocates one **sentinel** out of every
export pool the device created — DMA-BUF buffer, DMA-BUF image
(OPTIMAL), DMA-BUF image tiled (DRM modifier), OPAQUE_FD HOST_VISIBLE
buffer, OPAQUE_FD host-cached buffer, OPAQUE_FD DEVICE_LOCAL buffer,
OPAQUE_FD image — strictly before any caller can build a
`VkSwapchainKHR`, in
`HostVulkanDevice::allocate_one_sentinel_per_export_pool`. The device
holds them for its life (`HostVulkanDevice::export_pool_sentinels`),
on every driver. Each sentinel mirrors its pool's real frames (handle
type, memory flags, VMA host-access flags, image format and tiling)
so its memory-type bits admit the type the pool is pinned to.

All seven sentinels are intentionally **tiny** (8×8×4 = 256 bytes; the
image ones allocate an 8×8 `VkImage` — `B8G8R8A8_UNORM` for DMA-BUF,
`R8G8B8A8_UNORM` for OPAQUE_FD): empirical E2E on Cam Link 4K (run
during PR `fix/opaque-fd-export-sentinels-637`) showed a
consumer-resolution buffer sentinel (1920×1080×4 ≈ 8 MiB)
*deterministically* blocked the consumer's same-size post-swapchain
allocation, indicating NVIDIA tracks a cumulative byte budget on top
of the per-handle-type state. Sentinels exist only to pin the
per-handle-type kernel state, so they must not compete with
consumer-class allocations. Each sentinel frees its own allocation on
drop, and must drop before its pool: VMA's `vmaDestroyPool` over a
live dedicated allocation aborts with `Unfreed dedicated allocations
found!` — the same assertion text as an allocator teardown (#2247).
`HostVulkanDevice::Drop` clears the sentinels before destroying any
pool, and a sentinel allocation that returns early frees the sentinels
it already made.

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
becomes runnable and the retention can be re-validated. The
data-structure-level test
(`every_export_pool_the_device_created_keeps_a_tiny_sentinel_for_its_life`,
which includes `opaque_fd_image` in its expected labels when the pool
is constructed) is the regression lock.

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

Construction either yields a fully-usable `Arc<HostVulkanDevice>`
with every sentinel held, or fails — there is no half-formed state
observable to callers. Sentinels bypass the wrapper chain (raw
`vk::Buffer` / `vk::Image` + `vma::Allocation` + the device's
`Arc<vma::Allocator>`, not `HostVulkanBuffer` / `HostVulkanTexture`)
to avoid the `Arc<HostVulkanDevice>` back-reference cycle that would
prevent the device from ever dropping.

**Consumers do NOT need to pre-warm.** If you find yourself wanting
to allocate an exportable resource at processor `start()` time just to
anchor its handle type, don't: the engine's sentinels already do,
from before any of your code ran.
See CLAUDE.md's "Engine-wide defects get fixed at the engine layer" rule.

## Verifying / re-deriving the fix

Each protocol reverts one fix, runs a pipeline in which a consumer
allocates an OPAQUE_FD buffer
(`HostVulkanBuffer::new_opaque_fd_export_device_local`) after the
display's swapchain exists, then restores the fix and re-runs. The
failure is that allocation returning `A device memory allocation has
failed`. A healthy device logs one `HostVulkanDevice export pool
sentinel retained: <pool> (256 bytes)` line per export pool, e.g.
`opaque_fd_device_local`.

### A. Pre-warm-removed protocol (catches the original #624 bug)

Comment out the `allocate_one_sentinel_per_export_pool` call in
`HostVulkanDevice::new()`. The consumer's post-swapchain allocation
fails deterministically on vivid (`/dev/video2`). With the sentinels
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

Make `allocate_one_sentinel_per_export_pool` skip the OPAQUE_FD
sentinels, OR free them *before* any consumer can allocate (defeating
the long-lived purpose). Keep the DMA-BUF sentinels intact. On Cam Link 4K (`/dev/video0`) the
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
  by `vulkan_device::tests::every_export_pool_the_device_created_keeps_a_tiny_sentinel_for_its_life`.
