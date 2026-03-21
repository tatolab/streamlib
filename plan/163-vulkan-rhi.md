---
whoami: amos
name: "@github:tatolab/streamlib#163"
description: Linux — Vulkan RHI — complete the GPU backend
dependencies:
  - "up:@github:tatolab/streamlib#164"
  - "up:@github:tatolab/streamlib#165"
  - "up:@github:tatolab/streamlib#166"
  - "up:@github:tatolab/streamlib#167"
  - "up:@github:tatolab/streamlib#178"
adapters:
  github: builtin
---

@github:tatolab/streamlib#163

Phase 1 of the Linux support plan. **CLOSED — core work complete.**

### Completed (PRs #174, #176, fix/linux-compilation)
- Memory type selection via `find_memory_type()`
- VulkanPixelBuffer — HOST_VISIBLE staging with mapped CPU pointer
- VulkanBlitter — `blit_copy()` via `vkCmdCopyBuffer`
- VulkanPixelBufferPool — pre-allocation + ring-cycling with `Arc<VulkanPixelBuffer>` reuse
- VulkanTextureCache — `VkImageView` caching
- DMA-BUF — `export_dma_buf_fd()` and `from_dma_buf_fd()` on VulkanTexture
- `RhiPixelBufferRef` uses `Arc<VulkanPixelBuffer>` (no clone panic)
- Pool creation wired into `GpuContext` on Linux
- `PixelFormat` field on `VulkanPixelBuffer`

### Moved to downstream issues
- 1.6 Format converter → #167 (FFmpeg, prerequisite sub-task)
- 1.7 `StreamTexture::native_handle()` + `RhiPixelBufferExport/Import` → #164 (Broker, sub-tasks 2.5/2.6)
- 1.8 gpu-allocator → #181 (new issue, low priority)
- Cross-platform PixelFormat → #178 (new issue, blocks #166 and #167)
