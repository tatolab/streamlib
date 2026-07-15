# linux-media-expert — symptom index

Knowledge lives in `docs/`; this file is only routing. Update in the same PR that adds a learning (see `.claude/rules/docs-policy.md`).

Match your symptom, read the doc, then verify its claims against current code and a live device probe — a learning is the best-known state when it was written, not ground truth.

| symptom / trigger | read |
|---|---|
| Validating a camera→display change end-to-end without a window or physical hardware; virtual camera (v4l2loopback) + AI-readable PNG sampling setup | `docs/learnings/camera-display-e2e-validation.md` |
| Headless NVIDIA Vulkan in a container reports `ERROR_INCOMPATIBLE_DRIVER` / "Found no drivers" while `nvidia-smi`/CUDA work (missing GLVND/EGL dispatch); or userspace audio (PipeWire/ALSA/cpal) with no sound hardware in a container | `docs/learnings/headless-nvidia-vulkan-container.md` |
| Importing a DMA-BUF as a GL render target on NVIDIA fails (`GL_INVALID_OPERATION` 0x0502 / `GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` 0x8CD6); need the DRM-modifier `external_only` probe to pick a tiled (render-target-capable) modifier | `docs/learnings/nvidia-egl-dmabuf-render-target.md` |
| Intermittent fake OOM on an OPAQUE_FD allocation after a camera-path change, only on real UVC hardware — a FAILED cross-device DMA-BUF import probe perturbs NVIDIA's per-handle-type accounting (vivid/loopback never reproduce) | `docs/learnings/nvidia-opaque-fd-after-swapchain.md` |
