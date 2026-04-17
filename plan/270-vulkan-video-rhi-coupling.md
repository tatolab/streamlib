---
whoami: amos
name: Couple vulkan-video to streamlib RHI
status: pending
description: Refactor libs/vulkan-video to use streamlib's VulkanDevice and shared VMA allocator instead of managing its own Vulkan resources. Full RHI boundary compliance.
github_issue: 270
dependencies:
  - "down:@github:tatolab/streamlib#254"
adapters:
  github: builtin
---

@github:tatolab/streamlib#270

## Branch

Create `refactor/vulkan-video-rhi-coupling` from `main` (after #254 merges).

## Steps

1. Modify `VideoContext::new()` to accept an external `Arc<vma::Allocator>` from streamlib's `VulkanDevice`
2. Remove internal allocator creation from `VideoContext`
3. Route DPB image, bitstream buffer, and staging buffer creation through `VulkanDevice` RHI methods
4. Ensure all `vkCreate*` / `vmaCreate*` calls go through the RHI — no direct Vulkan API usage from the video crate
5. Update processor wrappers to pass the RHI allocator through
6. Verify encode/decode roundtrip still works after refactor
7. Run full test suite: `cargo test -p vulkan-video` + `cargo test -p streamlib`

## Scope

- `libs/vulkan-video/src/video_context.rs` — accept external allocator
- `libs/vulkan-video/src/encode/` — use passed-in allocator for all resource creation
- `libs/vulkan-video/src/decode/` — same
- `libs/vulkan-video/src/codec_utils/` — same
- `libs/streamlib/src/linux/processors/` — encoder/decoder wrappers pass RHI allocator
- `libs/streamlib/src/vulkan/rhi/vulkan_device.rs` — expose allocator handle if needed
