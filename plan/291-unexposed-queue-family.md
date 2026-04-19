---
whoami: amos
name: vkGetDeviceQueue called with unexposed family
status: completed
description: Ensure every queue family whose queue we later fetch is requested at VkDeviceQueueCreateInfo time.
github_issue: 291
adapters:
  github: builtin
---

@github:tatolab/streamlib#291

## Branch

Create `fix/unexposed-queue-family` from `main`.

## Steps

1. In `libs/streamlib/src/vulkan/rhi/vulkan_device.rs`, collect every family index we later call `get_device_queue` on (graphics, transfer, compute, video encode, video decode).
2. Verify each one is present in the `VkDeviceQueueCreateInfo` array used at device creation.
3. Deduplicate: a single family may satisfy multiple roles (graphics+transfer on some GPUs).
4. Validate with `VK_LOADER_LAYERS_ENABLE="*validation*"` at init.

## Verification

- `VUID-vkGetDeviceQueue-queueFamilyIndex-00384` silent at device init.
