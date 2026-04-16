# Hard-won learnings

This directory captures **specific, non-obvious** discoveries made while
working on streamlib — things you would NOT have known from reading the
code, the Vulkan spec, or generic Rust patterns.

Each file documents:
- The exact symptom that triggers the lookup
- Specific root cause
- Concrete fix pattern (with code)
- Reference to the commit / file where it lives

If you're about to add a generic note like "be careful with X" or "the
codebase uses Y" — DON'T. Those go in CLAUDE.md or the code itself. This
directory is for the surprises.

## Index

- [@docs/learnings/nvidia-dma-buf-after-swapchain.md](nvidia-dma-buf-after-swapchain.md) —
  `VK_ERROR_OUT_OF_DEVICE_MEMORY` from `vmaCreateImage`/`vkAllocateMemory`
  on NVIDIA Linux when a swapchain exists
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
