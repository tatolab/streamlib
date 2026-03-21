---
whoami: amos
name: "@github:tatolab/streamlib#178"
description: Linux — Cross-platform PixelFormat enum
dependencies:
  - "down:@github:tatolab/streamlib#163"
  - "up:@github:tatolab/streamlib#166"
  - "up:@github:tatolab/streamlib#167"
adapters:
  github: builtin
---

@github:tatolab/streamlib#178

Foundational change: make the `PixelFormat` enum available on all platforms.

### AI context (2026-03-21)
- Currently `PixelFormat` is macOS-only (`core/rhi/pixel_format.rs`). Linux has `{ Unknown }`.
- `gpu_context.rs` hardcodes `4u32` bpp because it can't derive from `Unknown` format.
- The macOS enum uses CVPixelFormatType u32 discriminants — these work as platform-agnostic identifiers.
- Methods like `bits_per_pixel()`, `is_yuv()`, `is_rgb()`, `plane_count()` are pure logic, not platform-specific.
- Only `as_cv_pixel_format_type()` and `from_cv_pixel_format_type()` need macOS gating.

### What to do
- Move enum variants out of `#[cfg(target_os = "macos")]` in `core/rhi/pixel_format.rs`
- Keep CVPixelFormatType conversion methods macOS-gated
- Remove the `#[cfg(not(target_os = "macos"))] enum PixelFormat { Unknown }` stub
- Update `gpu_context.rs` to derive bpp from format instead of hardcoding 4
- Update `VulkanPixelBufferPool::new()` to use `format.bits_per_pixel() / 8`

### Files to modify
- `libs/streamlib/src/core/rhi/pixel_format.rs` — main change
- `libs/streamlib/src/core/context/gpu_context.rs` — remove hardcoded bpp
- `libs/streamlib/src/vulkan/rhi/vulkan_pixel_buffer_pool.rs` — derive bpp from format

### Blocks
- #166 (Linux processors) — need real format variants
- #167 (FFmpeg codecs) — need NV12, RGBA format awareness

### Depends on
- #163 (Vulkan RHI) — complete
