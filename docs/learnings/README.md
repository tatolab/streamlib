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
- [@docs/learnings/headless-nvidia-vulkan-container.md](headless-nvidia-vulkan-container.md) —
  Headless NVIDIA Vulkan in a container reports `ERROR_INCOMPATIBLE_DRIVER` without the GLVND/EGL dispatch libs (the toolkit mounts only vendor libs); plus the PipeWire-in-container recipe for an ALSA/cpal app (virtual null sink, `XDG_RUNTIME_DIR=/run/user/0`, poll-don't-sleep)
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
- [@docs/learnings/cdylib-make-borrow-cached-fields.md](cdylib-make-borrow-cached-fields.md) —
  Plugin pipeline runs end-to-end clean but produces zero/black output
  when host-side `make_*_borrow` helpers leave the PluginAbiObject's cached
  POD fields zeroed instead of mirroring the inner
- [@docs/learnings/slpkg-raw-device-rhi-construction.md](slpkg-raw-device-rhi-construction.md) —
  GPU package works in-process but crashes the driver (NVIDIA double-free in
  `vkCreatePipelineLayout`) as a separately-built `.slpkg` — it hand-rolled
  RHI on the raw `HostVulkanDevice` (via `host_vulkan_device_arc()`) instead
  of the cdylib-safe FullAccess primitives; non-`#[repr(C)]` layout differs
  across separate builds. Version-alignment doesn't fix it
- [@docs/learnings/nvidia-dual-vulkan-device-crash.md](nvidia-dual-vulkan-device-crash.md) —
  SIGSEGV when two Vulkan devices have concurrent GPU work on NVIDIA Linux
- [@docs/learnings/cross-process-vkimage-layout.md](cross-process-vkimage-layout.md) —
  `VkImageLayout` is independent state per `VkDevice` by Vulkan spec; cross-
  process layout coordination flows via application protocol + QFOT
  release/acquire (with `VK_EXT_external_memory_acquire_unmodified` chained
  for content preservation), bridging `UNDEFINED → target` as the fallback
  when extensions are missing
<!-- polyglot-venv-registry-env learning removed 2026-07-12 (#1245) — the
     hosted-daemon backend it described (and its registry-token / daemon-URL
     env surface) was dropped when the static file tree became the only
     registry (see docs/architecture/static-registry.md). The venv build now
     derives `UV_INDEX` from the tree-root `STREAMLIB_REGISTRY_URL`; the
     tokenless read shape means there is no token env var to forget. -->
- [@docs/learnings/sandboxing-demo-content-pending-engine-feature.md](sandboxing-demo-content-pending-engine-feature.md) —
  Recipe for relocating app-specific hot-path content out of the engine
  into an example crate when the right engine primitive is a future
  feature: gated boundary-check exception + heavy module-level docs +
  follow-up ticket blocked by the destination
- [@docs/learnings/vulkanalia-acceleration-structure-instance-layout.md](vulkanalia-acceleration-structure-instance-layout.md) —
  `vulkanalia-sys` 0.35.0's `VkAccelerationStructureInstanceKHR` field order
  disagrees with the Vulkan C spec; using the struct directly puts the BLAS
  device address at the wrong offset and every TLAS instance points at
  garbage. Workaround: serialize the 64-byte instance manually in spec order
- [@docs/learnings/concurrent-vkdevicewaitidle-threading.md](concurrent-vkdevicewaitidle-threading.md) —
  Concurrent `vkDeviceWaitIdle` on NVIDIA SIGSEGVs in `libnvidia-glcore`
  during multi-processor GPU setup — it's externally synchronized over the
  device + every queue it owns. The validation layer
  (`UNASSIGNED-Threading-Info: Couldn't find VkQueue`) is the diagnostic that
  cracks an otherwise-causeless driver crash; the fix routes every wait
  through `HostVulkanDevice::wait_idle` (holds all queue mutexes), enforced by
  `xtask check-device-wait-idle`
- [@docs/learnings/startup-crash-iceoryx2-wire-vs-gpu-setup-race.md](startup-crash-iceoryx2-wire-vs-gpu-setup-race.md) —
  Two different NVIDIA-Linux startup SIGSEGVs both exit 139: an iceoryx2 WIRE
  crash (`DoesNotSupportRequestedMinBufferSize`, never reaches `setup()`) and a
  latent GPU concurrent-setup race (`vkCreateComputePipelines` in glcore, during
  `setup()`). Classify by `grep "Calling setup"` before blaming GPU concurrency;
  gdb/api_dump/validation overhead shifts *which* crash you hit, so never use
  them to decide the production failure mode; re-verify the symptom after each
  fix
