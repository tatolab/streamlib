---
whoami: amos
name: '@github:tatolab/streamlib#164'
description: Linux — Broker backend — Unix sockets + DMA-BUF fd passing
adapters:
  github: builtin
blocked_by:
- '@github:tatolab/streamlib#166'
blocks:
- '@github:tatolab/streamlib#163'
- '@github:tatolab/streamlib#180'
---

@github:tatolab/streamlib#164

Phase 2 of the Linux support plan. Cross-process GPU surface sharing on Linux.

### AI context (2026-03-21)
- VulkanTexture has working `export_dma_buf_fd()` / `from_dma_buf_fd()` but `StreamTexture::native_handle()` returns None — needs VulkanDevice threaded through
- `RhiPixelBufferExport/Import` traits return `NotSupported` on Linux — must implement for buffer-level DMA-BUF sharing
- `RhiPixelBufferRef` uses `Arc<VulkanPixelBuffer>` — cloning is safe, no more panic
- `PixelFormat` is `Unknown` on Linux — #178 may be needed if broker does format-aware ops

### Key work
- `unix_socket_service.rs` — new broker listener for Linux
- `surface_store.rs` — replace `Err(NotSupported)` stubs with Unix socket client
- Thread `VulkanDevice` through `StreamTexture` for `native_handle()` DMA-BUF export
- Implement `RhiPixelBufferExport/Import` for DMA-BUF (`core/rhi/external_handle.rs`)
- Factor gRPC diagnostic service out of `#[cfg(target_os = "macos")]`
- systemd user service for broker launch

### Depends on
#163 (Vulkan RHI) — complete
