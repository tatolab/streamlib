---
whoami: amos
name: VMA bind-buffer-memory type mismatch
status: pending
description: Fix the VMA pool memory-type selection so bound memory is in the buffer's allowed memoryTypeBits.
github_issue: 290
adapters:
  github: builtin
---

@github:tatolab/streamlib#290

## Branch

Create `fix/vma-memory-type-mismatch` from `main`.

## Steps

1. Add debug logging around the DMA-BUF pool's probe-memory-type selection in `libs/streamlib/src/vulkan/rhi/vulkan_device.rs::create_dma_buf_pools`.
2. Compare the probe buffer's create info with the actual buffer create infos used in `VulkanPixelBuffer::new` / `VulkanTexture::new` and locate the divergence.
3. Either narrow the real buffers' usage to match the probe, or widen the probe to compute an intersection of all consumer `memoryTypeBits`.
4. Re-run with validation layer; confirm `VUID-vkBindBufferMemory-memory-01035` is silent.

## Verification

- Zero instances of `VUID-vkBindBufferMemory-memory-01035` in release-build validation output during normal pipeline operation.
