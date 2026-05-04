# Hard-won learnings

This directory captures **specific, non-obvious** discoveries made while
working on streamlib — things you would NOT have known from reading the
code, the Vulkan spec, or generic Rust patterns.

## These notes are not authoritative

Learnings are a conversation between past-me and current-me, not a spec.
Drivers update, code refactors, assumptions shift. Be skeptical:

- **Trust what you observe now over what's written here.** If reality
  disagrees with a learning, the learning is probably out of date.
- **Edit, rewrite, or delete freely.** A learning may be wrong, too
  narrow, too broad, or simply no longer relevant. Changing it is part
  of normal PR scope — no special approval needed.
- **Verify before applying.** A learning that says "do X" is a
  hypothesis for your current situation, not a command. Confirm the
  trigger still matches and re-derive the conclusion when the stakes
  are non-trivial.

See CLAUDE.md's "Hard-won learnings" section for the full framing.

## What makes a good learning

Each file should document:
- The **exact symptom** that triggers the lookup (specific error
  strings, VUIDs, failure patterns — specific enough that a search
  matches when it should)
- The **underlying driver/library/spec constraint** — the invariant
  that survives refactors, not the line number that tripped over it
- A **concrete fix pattern** (with code, in terms of the constraint)
- **Orientation links** to the files where the pattern currently lives
  (for navigation, not as the load-bearing truth — those files will
  move)

Avoid the two failure modes:

- **Too generic** (`be careful with X`, `the codebase uses Y`) — those
  go in CLAUDE.md or the code itself. This directory is for surprises.
- **Too specific to one spot** (`edit line 137 of file X`) — the fix
  will be wrong within a month. Write lessons that hold even after the
  surrounding code is renamed or restructured.

## Index

- [@docs/learnings/nvidia-dma-buf-after-swapchain.md](nvidia-dma-buf-after-swapchain.md) —
  `VK_ERROR_OUT_OF_DEVICE_MEMORY` from `vmaCreateImage`/`vkAllocateMemory`
  on NVIDIA Linux when a swapchain exists
- [@docs/learnings/nvidia-opaque-fd-after-swapchain.md](nvidia-opaque-fd-after-swapchain.md) —
  Same NVIDIA cap as DMA-BUF, but for OPAQUE_FD allocations (CUDA / OpenCL
  interop); engine pre-warms every export pool at `HostVulkanDevice` construction
- [@docs/learnings/nvidia-egl-dmabuf-render-target.md](nvidia-egl-dmabuf-render-target.md) —
  Linear DMA-BUFs on NVIDIA are EGL `external_only=TRUE` (sampler-only); FBO color attachments require a tiled DRM modifier from `eglQueryDmaBufModifiersEXT`
- [@docs/learnings/vma-export-pools.md](vma-export-pools.md) —
  Pattern for mixing DMA-BUF exportable and non-exportable allocations via VMA pools
- [@docs/learnings/vulkan-frames-in-flight.md](vulkan-frames-in-flight.md) —
  `MAX_FRAMES_IN_FLIGHT = 2`, NOT `swapchain.images.len()`
- [@docs/learnings/camera-display-e2e-validation.md](camera-display-e2e-validation.md) —
  Validate camera→display end-to-end via virtual camera + PNG sampling
- [@docs/learnings/vulkanalia-empty-slice-cast.md](vulkanalia-empty-slice-cast.md) —
  Cryptic `Cast` trait error when passing `&[]` to vulkanalia Vulkan methods
- [@docs/learnings/pubsub-lazy-init-silent-noop.md](pubsub-lazy-init-silent-noop.md) —
  Test hangs indefinitely because PUBSUB silently no-ops without `init()`
- [@docs/learnings/nvidia-dual-vulkan-device-crash.md](nvidia-dual-vulkan-device-crash.md) —
  SIGSEGV when two Vulkan devices have concurrent GPU work on NVIDIA Linux
- [@docs/learnings/cross-process-vkimage-layout.md](cross-process-vkimage-layout.md) —
  `VkImageLayout` is independent state per `VkDevice` by Vulkan spec; cross-
  process layout coordination flows via application protocol + QFOT
  release/acquire (with `VK_EXT_external_memory_acquire_unmodified` chained
  for content preservation), bridging `UNDEFINED → target` as the fallback
  when extensions are missing
- [@docs/learnings/vulkanalia-acceleration-structure-instance-layout.md](vulkanalia-acceleration-structure-instance-layout.md) —
  `vulkanalia-sys` 0.35.0's `VkAccelerationStructureInstanceKHR` field order
  disagrees with the Vulkan C spec; using the struct directly puts the BLAS
  device address at the wrong offset and every TLAS instance points at
  garbage. Workaround: serialize the 64-byte instance manually in spec order
