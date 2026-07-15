---
paths:
  - "runtime/streamlib-engine/src/vulkan/**"
  - "runtime/streamlib-consumer-rhi/**"
  - "adapters/**"
  - "packages/*/src/linux/**"
---

# RHI boundary

- **Nothing outside the RHI touches `vulkanalia`.** Host-side RHI is
  `runtime/streamlib-engine/src/vulkan/rhi/`; the consumer-side carve-out is
  `runtime/streamlib-consumer-rhi/`. Together they own every `vulkanalia` call. Processors,
  codecs, adapters go through `GpuContext`, never raw Vulkan or `HostVulkanDevice`. `ash` is gone;
  never reintroduce it. `cargo xtask check-boundaries` enforces.
- **One kernel abstraction per pipeline kind** (`VulkanComputeKernel`, graphics, ray-tracing).
  Construct via `GpuContext::create_*` / `GpuContextFullAccess::create_*` — never
  `VulkanComputeKernel::new` on a raw device (unsound in a separately-built `.slpkg`). Declare
  bindings as data; never hand-roll a descriptor set / pool / pipeline layout / command buffer /
  fence.
- **`TextureRegistration` is the single per-surface lifecycle record**, keyed by `surface_id` in
  `GpuContext::texture_cache` — extend it, never spin up a parallel `HashMap<surface_id, …>`.
- **`TextureRing` is the single decode-output / CPU-upload rotating-texture abstraction** — never
  hand-roll a `Vec<Texture>` + index for that use case.
- Subprocess Vulkan is import-side only (FD import + bind + map + layout transitions + timeline
  wait/signal); everything privileged escalates.

Read before you change:
- allocation / export / VMA config → `docs/learnings/vma-export-pools.md`,
  `nvidia-dma-buf-after-swapchain.md`, `nvidia-opaque-fd-after-swapchain.md`.
- any device-wait path → `docs/learnings/concurrent-vkdevicewaitidle-threading.md`.
- sizing per-frame resources → `docs/learnings/vulkan-frames-in-flight.md`.
