---
whoami: amos
name: '@github:tatolab/streamlib#288'
adapters:
  github: builtin
description: Camera ring textures missing TRANSFER_SRC_BIT — Add `TextureUsages::COPY_SRC` to the camera ring texture descriptor so the cmd_copy_image_to_buffer path is spec-valid.
github_issue: 288
---

@github:tatolab/streamlib#288

## Branch

Create `fix/camera-ring-copy-src` from `main`.

## Steps

1. In `libs/streamlib/src/linux/processors/camera.rs`, locate the ring `TextureDescriptor` (search `STORAGE_BINDING | TextureUsages::TEXTURE_BINDING` near the ring allocation).
2. Add `| TextureUsages::COPY_SRC`.
3. Re-run with validation layer; confirm `VUID-VkImageMemoryBarrier2-oldLayout-01212` and `VUID-vkCmdCopyImageToBuffer-srcImage-00186` are gone.

## Verification

- Validation layer silent on the two VUIDs during camera steady-state.
- Visual output unchanged.
