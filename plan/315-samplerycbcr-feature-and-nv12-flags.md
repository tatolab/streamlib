---
whoami: amos
name: '@github:tatolab/streamlib#315'
adapters:
  github: builtin
description: Enable samplerYcbcrConversion feature and audit NV12 image-create flags — Turn on the samplerYcbcrConversion device feature in VulkanDevice and audit the encoder-src NV12 image / image-view flags so VUID-vkCreateSamplerYcbcrConversion-None-01648, VUID-VkImageCreateInfo-pNext-06811, and VUID-VkImageViewCreateInfo-usage-02275 go silent.
github_issue: 315
---

@github:tatolab/streamlib#315

## Branch

Create `fix/samplerycbcr-feature-and-nv12-flags` from `main`.

## Steps

1. In `libs/streamlib/src/vulkan/rhi/vulkan_device.rs` at device creation:
   - Chain `VkPhysicalDeviceVulkan11Features { samplerYcbcrConversion = VK_TRUE, .. }` into `VkPhysicalDeviceFeatures2` passed via `VkDeviceCreateInfo::pNext`.
   - Confirm no other Vulkan 1.1/1.2/1.3/1.4 features chain drops it accidentally.
2. Audit NV12 (`VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`) image + image-view creation sites in `libs/vulkan-video/`:
   - `src/rgb_to_nv12.rs` (encoder source)
   - `src/encode/staging.rs`
   - `src/encode/session.rs`
   - `src/vk_video_decoder/vk_video_decoder.rs`
   Match each create flag / usage / view-usage against what
   `vkGetPhysicalDeviceVideoFormatPropertiesKHR` reports for the active profile. Drop unsupported combos (notably `STORAGE_BIT` on views that do not need it) or split into separate views.
3. Re-run `vulkan-video-roundtrip h264 /dev/video2 15` and `h265` under `VK_LOADER_LAYERS_ENABLE=*validation*` and confirm zero instances of each of the three target VUIDs.

## Verification

- Validation layer silent on `VUID-vkCreateSamplerYcbcrConversion-None-01648`, `VUID-VkImageCreateInfo-pNext-06811`, `VUID-VkImageViewCreateInfo-usage-02275` across a 300-frame run.
- Encoded output is still a valid bitstream (encoder and decoder frame counts match).
- No regression on `VUID-VkImageViewCreateInfo-format-06415` (the VUID #289 fixed).

## Context

Discovered during #296 E2E validation sweep. Not a regression of #289 — #289 fixed a neighboring VUID (`-06415`) on image-view creation; this issue fixes the underlying device-feature + format-flag hygiene that `-01648`, `-06811`, `-02275` are complaining about. Adjacent to but distinct from #300 (encoder-src *profile* chaining).
