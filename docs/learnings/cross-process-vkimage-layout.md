# Cross-process `VkImageLayout` coordination

## When you need this

You're consuming a `VkImage` imported from another process (DMA-BUF
+ `vkImportMemoryFdKHR` + `vkBindImageMemory`) and the producer
left the image in some non-`UNDEFINED` layout (`SHADER_READ_ONLY_OPTIMAL`,
`COLOR_ATTACHMENT_OPTIMAL`, `GENERAL`, etc.). Your consumer side wants to
issue a barrier with `oldLayout = <producer's layout>` and the validation
layer is rejecting it. Or you do barrier from `UNDEFINED` and you're
worried about content discard.

## The spec gotcha

Per Vulkan spec
([VkImageCreateInfo](https://registry.khronos.org/vulkan/specs/latest/html/vkspec.html#VkImageCreateInfo)),
`initialLayout` must be `VK_IMAGE_LAYOUT_UNDEFINED` or `_PREINITIALIZED`.
There is no "import this `VkImage` already in layout L" form.

Every freshly-created `VkImage` in the consumer's process — including
ones bound to `vkImportMemoryFdKHR`-imported memory — starts at the
declared `initialLayout = UNDEFINED`. The consumer's layout state is
its **own state machine**, independent of the producer's by spec
construction. There is no shared mutable layout tracker across the
import boundary.

So:

- A consumer barrier with `oldLayout = SHADER_READ_ONLY_OPTIMAL` against
  a freshly-imported `VkImage` whose tracker is `UNDEFINED` trips
  `VUID-VkImageMemoryBarrier-oldLayout-01197` ("oldLayout must be either
  `VK_IMAGE_LAYOUT_UNDEFINED` or the current layout of the image
  subresource(s)").
- A consumer barrier with `oldLayout = UNDEFINED` is spec-legal but
  permits content discard. The "unmodified import" semantics aren't
  implicit.

## What works

Cross-process layout is communicated by **application protocol**, not by
shared mutable layout state. The Khronos
[`VK_EXT_external_memory_acquire_unmodified`](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html)
proposal states this directly:

> The solution should not require the implementation to internally
> track the `VkImageLayout` of external images, as such tracking can
> be complex to implement and cause performance overhead.

Two pieces — one core, one optional extension — make spec-correct
cross-process layout coordination work:

1. **`VK_QUEUE_FAMILY_EXTERNAL`** is the queue family index used in
   paired producer-side release / consumer-side acquire barriers to
   declare ownership transfer across the import boundary. **Core
   Vulkan 1.1** (promoted from `VK_KHR_external_memory`); always
   available on any device that supports external memory at all. The
   `VK_QUEUE_FAMILY_FOREIGN_EXT` constant from
   `VK_EXT_queue_family_foreign` is a generalization for non-Vulkan
   foreign owners (OpenGL, video codecs, etc.) — both work with
   acquire-unmodified, but `EXTERNAL` is the idiomatic choice for
   cross-process Vulkan-to-Vulkan and avoids an unnecessary extension
   dependency.
2. **`VK_EXT_external_memory_acquire_unmodified`** (optional EXT —
   not promoted to core through Vulkan 1.4) lets the consumer-side
   acquire barrier chain `VkExternalMemoryAcquireUnmodifiedEXT
   { acquireUnmodifiedMemory = VK_TRUE }`, which tells the
   implementation "the producer left the contents intact, please
   preserve them across this transfer." This turns a
   content-discard-permitted UNDEFINED equivalent into a
   content-preserving acquire.

The full QFOT pair:

```c
// Producer side, after writes complete:
VkImageMemoryBarrier2 release = {
    .srcStageMask = VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
    .srcAccessMask = VK_ACCESS_2_MEMORY_WRITE_BIT,
    .dstStageMask = VK_PIPELINE_STAGE_2_NONE,
    .dstAccessMask = 0,
    .oldLayout = <producer's current layout>,
    .newLayout = <published layout to consumer>,
    .srcQueueFamilyIndex = <producer's queue family>,
    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL,
    .image = vkImage,
    /* ... */
};

// Consumer side, before first use:
VkExternalMemoryAcquireUnmodifiedEXT unmodified = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_ACQUIRE_UNMODIFIED_EXT,
    .acquireUnmodifiedMemory = VK_TRUE,
};
VkImageMemoryBarrier2 acquire = {
    .pNext = &unmodified,  /* drives content-preservation semantics */
    .srcStageMask = VK_PIPELINE_STAGE_2_NONE,
    .srcAccessMask = 0,
    .dstStageMask = VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
    .dstAccessMask = VK_ACCESS_2_MEMORY_READ_BIT | VK_ACCESS_2_MEMORY_WRITE_BIT,
    .oldLayout = <published layout>,
    .newLayout = <consumer's target>,
    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL,
    .dstQueueFamilyIndex = <consumer's queue family>,
    .image = vkImage,
    /* ... */
};
```

The producer publishes the post-release `VkImageLayout` to the
consumer via application protocol — for streamlib that's the
surface-share IPC (per-surface `current_image_layout` field, and
optional per-frame `Videoframe.texture_layout` override).

## Bridging fallback

NVIDIA Linux drivers (production 570.211.01 and developer betas
through 595.44 / 596.46 as of 2026-05-03) do not expose
`VK_EXT_external_memory_acquire_unmodified`. Without it, the
consumer cannot chain `VkExternalMemoryAcquireUnmodifiedEXT` on a
QFOT acquire — issuing an acquire from `VK_QUEUE_FAMILY_EXTERNAL`
without the chain permits the implementation to discard contents.

The pragmatic fallback is a same-family `UNDEFINED → published_layout`
bridging transition: spec-correct (validation-clean), but content
discard is permitted. **In practice DMA-BUF kernel-side memory
contents are preserved on every modern Linux Vulkan driver** (NVIDIA
empirical via streamlib's E2E camera→Path-2-display flow; Mesa
iris/radeonsi follow the same convention). The bridging transition
aligns the consumer's layout tracker with the producer's published
layout so subsequent consumer barriers (`oldLayout = published →
target`) are validation-clean per VUID-01197.

**The bridging fallback is structurally permanent on NVIDIA, not
interim.** NVIDIA engineers contributed to the Khronos extension
proposal (per Khronos history), but as of 2026-05-03 the extension
is not in NVIDIA's published support list — neither production
drivers nor the latest developer betas. NVIDIA exposes adjacent
extensions like `VK_EXT_external_memory_dma_buf` and
`VK_EXT_external_memory_host` but not the acquire-unmodified one.
Mesa drivers (iris, radeonsi) are the eventual landing point for
the QFOT-acquire path.

The streamlib `acquire_from_foreign` helpers in `HostVulkanDevice`
and `ConsumerVulkanDevice` pick QFOT when the extension is present,
fall back to bridging otherwise, and expose the choice via
`VulkanRhiDevice::supports_qfot_acquire_unmodified` for callers that
want to surface the trade-off.

## NVIDIA empirical content preservation

NVIDIA Linux DMA-BUF imports preserve contents across
`UNDEFINED → X` transitions in practice — verified empirically on
the streamlib camera→Path-2-display flow with valid output frames
across thousands of cross-process consumer cycles. The kernel-side
DMA-BUF page cache survives the producer's release, regardless of
whether the consumer's `VkImage` tracker passes through UNDEFINED.

This is not a documented spec guarantee — it's a driver
implementation detail. NVIDIA isn't shipping
`VK_EXT_external_memory_acquire_unmodified` (see above), so on
NVIDIA the empirical preservation IS the long-term contract. AMD
and Intel via Mesa are unverified locally (no validated test
environment for those drivers as of 2026-05-03); when Mesa exposes
the extension, the QFOT-acquire path automatically takes effect for
those drivers, and the bridging fallback only ever runs on NVIDIA.

## How to detect the trip-wire in the field

Run with `VK_LOADER_LAYERS_ENABLE=*validation*` enabled and watch for:

- `VUID-VkImageMemoryBarrier2-oldLayout-01197` — the consumer's
  barrier `oldLayout` doesn't match the imported `VkImage`'s tracker.
  Fix: barrier from `UNDEFINED` first (or use QFOT acquire with
  `acquireUnmodifiedMemory`), THEN to your target layout.
- `VUID-vkCmdPipelineBarrier2-srcAccessMask-03904` — QFOT release
  barrier with `dstQueueFamilyIndex = FOREIGN_EXT` but the source
  access mask is non-zero. Fix: QFOT release sets
  `srcAccessMask = MEMORY_WRITE_BIT` (only); the dst side is `NONE` /
  `0` because there's no further access on the local queue.
- A black or magenta consumer frame with no validation error — the
  driver is silently discarding contents on the consumer's
  `UNDEFINED → X` transition. Expect this on AMD/Intel; not seen on
  NVIDIA.

## Reference

- Issue #633 — cross-process layout coordination implementation.
- Vulkan spec:
  [synchronization & queue-transfer](https://docs.vulkan.org/spec/latest/chapters/synchronization.html).
- Khronos proposal:
  [`VK_EXT_external_memory_acquire_unmodified`](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html).
- Sibling learning:
  [`docs/architecture/texture-registration.md`](../architecture/texture-registration.md)
  (engine-wide per-surface lifecycle state record + cross-process
  coordination layers).
- Sibling learning:
  [`docs/architecture/subprocess-rhi-parity.md`](../architecture/subprocess-rhi-parity.md)
  (the carve-out's QFOT machinery and how subprocess Vulkan code
  uses it).
- Implementation:
  `libs/streamlib-consumer-rhi/src/consumer_vulkan_device.rs`
  (`ConsumerVulkanDevice::release_to_foreign`,
  `acquire_from_foreign`, `supports_qfot_acquire_unmodified`),
  `libs/streamlib/src/vulkan/rhi/vulkan_device.rs` (host equivalents
  + the trait impl + the QFOT extension probe in `new()`).
