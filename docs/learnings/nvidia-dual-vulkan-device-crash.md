# NVIDIA Linux: SIGSEGV when two Vulkan devices have concurrent GPU work

## Symptom

Process crashes with SIGSEGV (exit code 139) when a second Vulkan
device is created while the first device has active GPU operations
(compute dispatches, buffer copies, video encode/decode sessions).

No error message — immediate segfault during `vkCreateDevice` or
`vmaCreateAllocator` on the second device.

**Failure pattern in streamlib:** CameraProcessor starts capturing
(GPU compute pipeline for NV12→RGBA conversion running on device A),
then H264/H265 EncoderProcessor calls `SimpleEncoder::new()` which
creates device B. The `vkCreateDevice` for device B crashes.

## Root cause

NVIDIA's Linux Vulkan driver appears to have thread-safety issues when
creating a new VkDevice while another VkDevice in the same process has
active command buffer submissions. The crash is inside the driver's
device creation path, not in user code.

This does NOT reproduce:
- When only one VkDevice exists (standalone encoder/decoder examples)
- When the second device is created before the first has active GPU work
- On Intel/AMD drivers (untested but architecturally expected to work)

## Fix

Share a single Vulkan device across all GPU consumers in the process.
In streamlib, this means the encoder/decoder must use the RHI's
VulkanDevice rather than creating their own.

See #270 (vulkan-video RHI coupling) — modifies `SimpleEncoder` /
`SimpleDecoder` to accept an external device + VMA allocator from
streamlib's GpuContext.

## Workaround (temporary)

Run encode/decode in a separate process that doesn't share the camera's
Vulkan device. This is the architecture for cross-process IPC via
iceoryx2 + the surface-share service, but adds latency and complexity.

## The engine's one device per process

`GpuDevice::new()` (`runtime/streamlib-engine/src/core/rhi/device.rs`) returns
the process's one GPU device: the first call creates it under a lock, and every
later or concurrent call returns that same device. Every `GpuContext` in a
process therefore shares one `HostVulkanDevice`. The same bug class also showed
up on MoltenVK: two test threads of one `cargo test` binary each made a
`GpuContext`, and the macOS CI lane crashed with SIGSEGV. A direct
`HostVulkanDevice::new()` call (some RHI tests make one) still creates a device
of its own.

Per-device test state now outlives a test that touches it, so a test that
changes the device hands back a guard that undoes the change when dropped
(`leave_no_room_in_any_frame_allocation_budget_for_this_test`).

## Reference
- Discovery: #254 (vulkan-video integration) — live camera pipeline
- Fix planned: #270 (vulkan-video RHI coupling)
- Tested on: NVIDIA GeForce RTX 3090, driver 570.x, Linux 6.17
