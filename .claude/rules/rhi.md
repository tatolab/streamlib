---
paths:
  - "runtime/streamlib-engine/src/vulkan/**"
  - "runtime/streamlib-consumer-rhi/**"
  - "adapters/**"
  - "runtime/streamlib-gpu-frame-import-client/**"
  - "runtime/streamlib-surface-client/**"
  - "runtime/streamlib-engine/src/core/runtime/output_port_viewer.rs"
---

# RHI boundary

- **Nothing outside the RHI touches `vulkanalia`.** Host-side RHI is
  `runtime/streamlib-engine/src/vulkan/rhi/`; the consumer-side carve-out is
  `runtime/streamlib-consumer-rhi/`. Together they own every `vulkanalia` call. Processors,
  codecs, adapters go through `GpuContext`, never raw Vulkan or `HostVulkanDevice`. `ash` is gone;
  never reintroduce it. `cargo xtask check-boundaries` enforces.
- **One kernel abstraction per pipeline kind** (`VulkanComputeKernel`, graphics, ray-tracing).
  Construct via `GpuContext::create_*` / `GpuContextFullAccess::create_*` — never
  `VulkanComputeKernel::new` on a raw device. Declare
  bindings as data; never hand-roll a descriptor set / pool / pipeline layout / command buffer /
  fence.
- **`TextureRegistration` is the single per-surface lifecycle record**, keyed by `surface_id` in
  `GpuContext::texture_cache` — extend it, never spin up a parallel `HashMap<surface_id, …>`.
- **`TextureRing` is the single decode-output / CPU-upload rotating-texture abstraction** — never
  hand-roll a `Vec<Texture>` + index for that use case.
- **GPU work belongs to the engine.** A process outside the runtime — a helper, Tato Studio's
  viewer — only imports what the engine hands it: FD import, bind, map, layout transitions,
  timeline wait/signal. When it needs more — an allocation, a copy, memory in another
  flavour — the engine does it on its own device and hands back something the process can
  import, the way a helper escalates an op to its parent. Processes that drove their own
  Vulkan crashed, and NVIDIA crashes when two devices submit concurrently in one process
  (`docs/learnings/nvidia-dual-vulkan-device-crash.md`). The one exception is a codec
  module's owned device, the only submitting device in its own process.

Read before you change:
- allocation / export / VMA config → `docs/learnings/vma-export-pools.md`,
  `nvidia-dma-buf-after-swapchain.md`, `nvidia-opaque-fd-after-swapchain.md`.
- any device-wait path → `docs/learnings/concurrent-vkdevicewaitidle-threading.md`.
- sizing per-frame resources → `docs/learnings/vulkan-frames-in-flight.md`.
