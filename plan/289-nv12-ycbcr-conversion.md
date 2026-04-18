---
whoami: amos
name: NV12 image views require VkSamplerYcbcrConversion
status: completed
description: Chain VkSamplerYcbcrConversionInfo into the pNext of every NV12-backed image view and sampler so spec requirements are met.
github_issue: 289
adapters:
  github: builtin
---

@github:tatolab/streamlib#289

## Branch

Create `fix/nv12-ycbcr-conversion` from `main`.

## Steps

1. Locate NV12 image-view creation sites (`VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`) — likely in `libs/vulkan-video/src/vk_video_decoder/vk_video_decoder.rs` and decoder session code.
2. Create a single `VkSamplerYcbcrConversion` per device (matching expected color range / space).
3. Chain `VkSamplerYcbcrConversionInfo { conversion }` into the `pNext` of every NV12 `VkImageViewCreateInfo` and `VkSamplerCreateInfo`.
4. Clean up the conversion on device destruction.

## Verification

- `VUID-VkImageViewCreateInfo-format-06415` silent during decoder startup and steady-state.
- Decoded output still visually correct.
