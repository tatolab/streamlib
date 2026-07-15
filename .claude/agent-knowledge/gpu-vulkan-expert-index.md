# gpu-vulkan-expert — symptom index

Knowledge lives in `docs/`; this file is only routing. Update in the same PR that adds a learning (see `.claude/rules/docs-policy.md`).

Match your symptom, read the doc, then verify its claims against current code — a learning is the best-known state when it was written, not ground truth.

| symptom / trigger | read |
|---|---|
| SIGSEGV in `libnvidia-glcore` via `vkCreateComputePipelines`/`vkCreateGraphicsPipelines` during concurrent multi-processor GPU setup; validation prints `UNASSIGNED-Threading-Info: vkDeviceWaitIdle(): Couldn't find VkQueue` | `docs/learnings/concurrent-vkdevicewaitidle-threading.md` |
| Startup exit 139 (SIGSEGV) and you can't tell GPU-race from an IPC/wire crash — decide via the "Calling setup" grep before blaming GPU concurrency | `docs/learnings/startup-crash-iceoryx2-wire-vs-gpu-setup-race.md` |
| Consumer barrier on an imported `VkImage` trips `VUID-VkImageMemoryBarrier-oldLayout-01197`; or a black/magenta cross-process consumer frame with no validation error | `docs/learnings/cross-process-vkimage-layout.md` |
| `VK_ERROR_OUT_OF_DEVICE_MEMORY` from `vmaCreateImage`/`vmaCreateBuffer`/`vkAllocateMemory` with a DMA-BUF export chain, after a swapchain exists — looks like OOM, isn't | `docs/learnings/nvidia-dma-buf-after-swapchain.md` |
| Same fake OOM but on the OPAQUE_FD export path (`new_opaque_fd_export*`, CUDA/OpenCL interop) after a swapchain exists | `docs/learnings/nvidia-opaque-fd-after-swapchain.md` |
| SIGSEGV creating a second `VkDevice` while the first device has active GPU work (no error message, crash in the driver's device-create path) | `docs/learnings/nvidia-dual-vulkan-device-crash.md` |
| Importing a DMA-BUF as an EGLImage: `glEGLImageTargetTexture2DOES` returns `GL_INVALID_OPERATION` (0x0502), or the FBO is `GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` (0x8CD6) — samples fine, won't render (linear modifier is `external_only=TRUE`) | `docs/learnings/nvidia-egl-dmabuf-render-target.md` |
| Mixing DMA-BUF-exportable and non-exportable allocations; deciding where to set the export handle types (global vs per-pool) | `docs/learnings/vma-export-pools.md` |
| Sizing per-frame Vulkan resources (semaphores, command buffers, descriptor sets, render-target rings) — image_count vs frames-in-flight | `docs/learnings/vulkan-frames-in-flight.md` |
| Building a TLAS and every `traceRayEXT` returns the miss shader, no validation error — acceleration-structure instance field order | `docs/learnings/vulkanalia-acceleration-structure-instance-layout.md` |
| Cryptic `cannot satisfy _: Cast` / `type annotations needed` passing `&[]` to a vulkanalia command (`cmd_pipeline_barrier` etc.) | `docs/learnings/vulkanalia-empty-slice-cast.md` |
| GPU package runs clean in-process but crashes the driver as a separately-built `.slpkg` (NVIDIA double-free in `vkCreatePipelineLayout` / `libnvidia-gpucomp`); version-alignment doesn't fix it | `docs/learnings/slpkg-raw-device-rhi-construction.md` |
| You need hot-path GPU work the engine has no clean primitive for yet and are tempted to put a kernel/wrapper in the engine or build the primitive ahead of the use case | `docs/learnings/sandboxing-demo-content-pending-engine-feature.md` |
| You changed GPU-pipeline code and need to confirm it renders (not black) end-to-end without a window or physical hardware | `docs/learnings/camera-display-e2e-validation.md` |
| Headless NVIDIA Vulkan in a container reports `ERROR_INCOMPATIBLE_DRIVER` / "Found no drivers" while `nvidia-smi`/CUDA work (missing GLVND/EGL dispatch) | `docs/learnings/headless-nvidia-vulkan-container.md` |
