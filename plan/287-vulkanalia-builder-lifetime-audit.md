---
whoami: amos
name: Vulkanalia builder lifetime audit across RHI and processors
status: completed
description: Audit every `.build()` + inline slice pattern in camera/display/vulkan-video/RHI and bind temporaries to locals so the driver sees valid memory.
github_issue: 287
adapters:
  github: builtin
---

@github:tatolab/streamlib#287

## Branch

Create `fix/vulkanalia-builder-lifetimes` from `main`.

## Steps

1. Grep for `.build()` in `libs/streamlib/src/linux/processors/*.rs`, `libs/streamlib/src/vulkan/rhi/*.rs`, `libs/vulkan-video/src/**/*.rs`.
2. At each site, check whether the returned struct is passed to another builder via `.xxx_infos(&[...])` / `.xxx(&[...])`. If the slice is inline, bind it to a `let`.
3. Verify under `VK_LOADER_LAYERS_ENABLE="*validation*"` on a release build that the sType-garbage / invalid-handle VUIDs listed in #287 are gone.
4. Consider wrapping raw vulkanalia submit/wait in a host-RHI helper that owns slice lifetimes.

## Verification

- Release-build vivid roundtrip runs with zero `VUID-VkCommandBufferSubmitInfo-*`, `VUID-VkSemaphoreSubmitInfo-*`, `VUID-VkSemaphoreWaitInfo-*`, `VUID-VkImageMemoryBarrier2-sType-sType`, or `VUID-VkImageMemoryBarrier2-*-parameter` errors during steady-state.
