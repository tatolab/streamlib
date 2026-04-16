# Per-frame Vulkan resources: size to MAX_FRAMES_IN_FLIGHT, not swapchain image_count

## Easy mistake to make

Naive Vulkan code (and one Vulkan tutorial in particular) sizes per-frame
resources to `swapchain.images.len()`:

```rust
// ❌ Wrong — over-allocates and ties two unrelated concerns together
let image_count = swapchain.images.len();  // typically 3-5
let semaphores: Vec<_> = (0..image_count).map(|_| create_semaphore()).collect();
let command_buffers = allocate(image_count);
let descriptor_sets = allocate(image_count);
let render_textures = allocate(image_count);
```

This is wrong. **Swapchain image count** is a presentation concern (how
many images the compositor wants in flight for double/triple buffering or
mailbox mode). **Frames in flight** is a CPU↔GPU pipelining concern (how
far the CPU can race ahead of the GPU). They are independent.

## Canonical pattern

```rust
const MAX_FRAMES_IN_FLIGHT: usize = 2;
```

| Resource | Size to | Rationale |
|---|---|---|
| Acquire-image semaphore | `MAX_FRAMES_IN_FLIGHT` | Per-frame sync |
| Render-finished semaphore | `MAX_FRAMES_IN_FLIGHT` | Per-frame sync |
| Command buffer | `MAX_FRAMES_IN_FLIGHT` | Per-frame recording |
| Descriptor set | `MAX_FRAMES_IN_FLIGHT` | Per-frame texture binding |
| Render-target ring texture | `MAX_FRAMES_IN_FLIGHT` | Per-frame WAR avoidance |
| Swapchain images | `image_count` (driver) | Per-image, driver-managed |
| Swapchain image views | `image_count` | Per-image, attaches to swapchain image |

Index per-frame resources with `current_frame ∈ [0, MAX_FRAMES_IN_FLIGHT)`.
Index per-image resources with `image_index` from `acquire_next_image_khr`.

## Why 2

- **Latency:** CPU runs at most 1 frame ahead of GPU → ~16ms input lag at
  60fps. With 4 frames in flight, lag balloons to ~50ms.
- **Memory:** Halves per-frame resource footprint vs naive 4.
- **NVIDIA gotcha:** Sidesteps NVIDIA's DEVICE_LOCAL allocation cap that
  triggers after swapchain creation
  (@docs/learnings/nvidia-dma-buf-after-swapchain.md). Asking for 2
  textures comfortably stays under the cap.
- **Industry standard:** Every major Vulkan tutorial and game engine uses
  2 (some use 3 for high-throughput rendering — never matched to
  image_count).

## Reference
- Refactor commit: `6816f54` `refactor(display): decouple frames-in-flight from swapchain image count`
- Implementation: `libs/streamlib/src/linux/processors/display.rs` (search `MAX_FRAMES_IN_FLIGHT`)
