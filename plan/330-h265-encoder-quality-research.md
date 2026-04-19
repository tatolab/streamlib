---
whoami: amos
name: "Research: H.265 encoder quality configuration on Vulkan Video / NVIDIA"
status: in_review
description: Audit the distinct quality-affecting knobs on our H.265 encode path (Vulkan API effort index vs. H.265 SPS profile/tier/level vs. QP/rate-control vs. tuning_mode) before #294 retest. #306's framing conflated several concepts; reviewer recalls a specific proper H.265 configuration fix. Research-only deliverable with a question list for interactive review.
github_issue: 330
adapters:
  github: builtin
dependencies:
  - "down:Vulkanalia builder lifetime audit across RHI and processors"
  - "down:Camera ring textures missing TRANSFER_SRC_BIT"
  - "down:NV12 image views require VkSamplerYcbcrConversion"
  - "down:VMA bind-buffer-memory type mismatch"
  - "down:vkGetDeviceQueue called with unexposed family"
  - "down:Cam Link encoder ERROR_OUT_OF_DEVICE_MEMORY in debug"
  - "down:Display render_finished semaphore must be per-swapchain-image"
  - "down:Encoder src picture profile mismatch"
  - "down:Decoder pool probe uses hard-coded resolution"
  - "down:Camera MMAP path sees 0 frames on v4l2loopback"
  - "down:Flaky H.265 decoder DEVICE_LOST during setup"
  - "down:Fixture-based PSNR rig for encoder/decoder roundtrips"
  - "down:Expose encoder quality_level with real-time default"
  - "down:Enable samplerYcbcrConversion feature and audit NV12 image-create flags"
  - "down:Swapchain-descriptor image in UNDEFINED layout at sample time"
---

@github:tatolab/streamlib#330

See the GitHub issue for full context. This task is research-only: the
deliverable is `docs/research/h265-encoder-quality-knobs.md` plus a
question list to review interactively with Jonathan. No encoder code
changes land here; any implementation is scoped as a follow-up that
this research gates.

Gating #294 retest so the retest doesn't run against a known-misframed
H.265 quality configuration.
