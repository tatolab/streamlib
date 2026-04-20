---
whoami: amos
name: '@github:tatolab/streamlib#279'
adapters:
  github: builtin
description: Retest camera + encoder + display roundtrip in release build — Validate full GPU pipeline (camera + encoder + decoder + display) in release build after synchronization fixes land. Confirms no SIGSEGV or OOM.
github_issue: 279
blocked_by:
- '@github:tatolab/streamlib#277'
- '@github:tatolab/streamlib#278'
---

@github:tatolab/streamlib#279

## Branch

Create `test/release-roundtrip-retest` from `main` (after #277 + #278 merge).

## Steps

1. H.264 roundtrip with Cam Link 4K: `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video0 30`
2. H.265 roundtrip with Cam Link 4K: `cargo run --release -p vulkan-video-roundtrip -- h265 /dev/video0 30`
3. Vivid virtual camera roundtrip: `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video10 30`
4. Dynamic processor add/remove: start camera-only, add encoder while running
5. Release vs debug parity: run all above in both modes
6. Document results and close retest items from #273 and #272/PR #275

## Retest Results — 2026-04-18

Executed on branch `test/release-roundtrip-retest` against `main` at commit 8faf8dc.
Vivid is at `/dev/video2` on this host, Cam Link at `/dev/video0`.
Logs archived at `/tmp/streamlib-279-retest/`.

### Test matrix

| # | Scenario | Build | Device | Exit | frames enc/dec | OOM | Outcome |
|---|----------|-------|--------|------|-----|-----|---------|
| 1 | H.264 | release | Cam Link | 139 | — | 0 | SIGSEGV |
| 2 | H.265 | release | Cam Link | 139 | — | 0 | SIGSEGV |
| 3 | H.264 | release | vivid    | 139 | — | 0 | SIGSEGV |
| 4 | H.264 | debug   | Cam Link | 0   | 0 / 0 | 890 | encoder OOM every frame |
| 5 | H.265 | debug   | Cam Link | 0   | 0 / 0 | 891 | same |
| 6 | H.264 | debug   | vivid    | 0   | 75 / 75 | 0 | **pass** |
| 7 | H.265 | debug   | vivid    | 0   | 75 / 75 | 0 | **pass** |

Dynamic add/remove was not attempted — release builds crash before any frames flow.

### Root cause — release SIGSEGV

**Bug pattern**: `vulkanalia` builder lifetime footgun. Calling `.build()` on a builder
consumes it and returns a plain `Vk*` struct, but that struct still holds raw pointers
into whatever slices the builder borrowed. When the source slice is a temporary
(inline `&[x]` literal or struct built by `.build()` and bound locally but then
sliced inline), the temporary is dropped at the end of the statement — the driver
then reads dangling pointers.

Debug preserves temporaries on the stack long enough that the driver happens to read
valid data before the slot is reused. Release LTO + optimizer reuses the stack slot
immediately, giving the driver garbage and causing NVIDIA's driver to SIGSEGV on
the next submit.

This was proven empirically: fixing the three sites below (and one ring-texture
usage bug) advances the crash from "immediately after swapchain creation" to
"camera→encoder→decoder all succeed, decoder first frame out, then crash in display
render path" — each fix unblocks the next frame's worth of work.

### Concrete sites (evidence-backed)

Vulkan validation layer output (run `VK_LOADER_LAYERS_ENABLE="*validation*"`)
surfaced the exact violations. Selected:

- `VUID-VkCommandBufferSubmitInfo-sType-sType` — sType is garbage
- `VUID-VkCommandBufferSubmitInfo-commandBuffer-parameter` — handle is garbage
- `VUID-VkSemaphoreSubmitInfo-sType-sType` — sType is garbage
- `VUID-VkSemaphoreSubmitInfo-semaphore-parameter` — handle is garbage
- `VUID-VkSemaphoreWaitInfo-pSemaphores-parameter` — handle is NULL

All four trace back to `.build()` + inline `&[...]` slice, e.g.
`libs/streamlib/src/linux/processors/camera.rs:1582-1593` for the camera's compute
submit, and `libs/streamlib/src/linux/processors/display.rs:869-877` for the
display's render submit. Fix is mechanical — bind the inner struct(s) and the
array to `let` bindings before passing to the outer builder:

```rust
// BROKEN — outer submit holds pointer into the temporary [cmd_info]
let cmd_info = vk::CommandBufferSubmitInfo::builder()...build();
let submit = vk::SubmitInfo2::builder()
    .command_buffer_infos(&[cmd_info])  // ← temporary array
    .build();
vulkan_device.submit_to_queue(queue, &[submit], fence);  // reads dangling ptr

// FIXED
let cmd_info = vk::CommandBufferSubmitInfo::builder()...build();
let cmd_infos = [cmd_info];
let submit = vk::SubmitInfo2::builder()
    .command_buffer_infos(&cmd_infos)
    .build();
```

Same pattern recurs throughout the codebase — validation surfaces more VUIDs
after each site is fixed (display `render_frame` has more barrier builders, etc.).

### Orthogonal bugs found

Independently of the lifetime pattern:

1. **Camera ring textures missing `TRANSFER_SRC_BIT`** — `camera.rs` allocates ring
   images with `STORAGE_BINDING | TEXTURE_BINDING` (→ Vulkan `STORAGE | SAMPLED`)
   and then `cmd_copy_image_to_buffer` on them, which requires `TRANSFER_SRC_BIT`.
   Validation: `VUID-vkCmdCopyImageToBuffer-srcImage-00186`,
   `VUID-VkImageMemoryBarrier2-oldLayout-01212`. Fix: add `TextureUsages::COPY_SRC`.

2. **NV12 image views without `VkSamplerYcbcrConversion`** —
   `VUID-VkImageViewCreateInfo-format-06415`, many sites in vulkan-video decoder
   path. Not the crash trigger but spec-invalid.

3. **Buffer memory type mismatch** — `VUID-vkBindBufferMemory-memory-01035`.
   VMA is binding memory from type 5 to buffers requiring `0x1b`. Needs RHI audit.

4. **Unexposed queue family requested** —
   `VUID-vkGetDeviceQueue-queueFamilyIndex-00384` — code calls `vkGetDeviceQueue`
   with a family that wasn't enabled in `VkDeviceQueueCreateInfo`.

### Root cause — Cam Link debug OOM (#4, #5)

`ERROR_OUT_OF_DEVICE_MEMORY` on every encoder `process()`, only on Cam Link.
Cam Link goes through the UVC/MMAP+memcpy camera path. Vivid uses NV12 direct
capture with no memcpy. The MMAP path allocates extra host-visible buffers per
frame that push the device over NVIDIA's DMA-BUF budget
(see @docs/learnings/nvidia-dma-buf-after-swapchain.md). Likely interacts with
the encoder's `create_image`/`create_buffer` under `with_device_resource_lock`
failing when the budget is already exhausted.

Separate issue from the SIGSEGV and not investigated past OOM confirmation.

### Conclusion

#273 / #277 / #278 were correct as far as they went (adding CPU-side
synchronization around GPU submits) but they address a different bug than
what's actually crashing the release builds today. The real culprit is a
pervasive vulkanalia builder-lifetime bug that only surfaces under release
optimization.

### Recommended follow-up issues

1. **fix(rhi): vulkanalia builder lifetime bugs across camera/display/vulkan-video**
   — audit every call site that uses `.build()` + inline slice; bind to `let`.
   Add a clippy lint or wrap vulkanalia with a safer API if feasible.

2. **fix(camera): add `COPY_SRC` to ring texture usage** — one-line usage flag
   change; resolves the image-layout VUIDs flagged by validation.

3. **fix(rhi): NV12 image views need VkSamplerYcbcrConversion** — affects
   vulkan-video decoder output handling.

4. **fix(rhi): memory-type mismatch in VMA-backed buffer allocation** — audit
   the `vkBindBufferMemory` call path.

5. **fix(rhi): vkGetDeviceQueue requesting unexposed family** — queue-family
   selection bug during device init.

6. **investigate(camera): Cam Link encoder OOM in debug** — separate from the
   SIGSEGV; likely DMA-BUF budget issue on the MMAP+memcpy path.

7. **test(ci): run all tests and examples with Vulkan validation layer enabled**
   — this entire class of bug would have been caught earlier.
