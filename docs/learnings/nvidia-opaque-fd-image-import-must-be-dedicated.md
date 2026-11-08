# An OPAQUE_FD image import must be dedicated to its image on NVIDIA

## Symptom

A consumer imports a pool of OPAQUE_FD `VkImage`s another device or process
exported, and one of them samples as all zeros — a uniform black picture —
while every other import of the same pool reads correctly. No validation
error, no failed call. Which import goes black is deterministic for a given
sequence of imports and repeats on every reuse of that import; re-importing
the memory for every frame hides it.

Seen on an RTX 3090, driver 595.99.02: `H264Encoder` fed a 60 fps camera
encoded one black P-frame each time the camera's 22-slot output pool came
round to its 18th slot (2423 bytes at 1080p, every time). A hardware test
that clears 32 1080p OPAQUE_FD images to known colours and copies each back
on a consumer device holding all 32 imports reads one of them as
`[0, 0, 0, 0]` across every pixel.

## Constraint

The exporter allocates each OPAQUE_FD image as a dedicated allocation
(`VMA_ALLOCATION_CREATE_DEDICATED_MEMORY`). On NVIDIA a dedicated image
allocation carries that image's device-specific memory state — its
compression kind among it. An import whose `VkMemoryAllocateInfo` names no
`VkMemoryDedicatedAllocateInfo` is a plain allocation; binding the image to it
lets the driver pick a layout of its own, which for some images is not the
exporter's, and the consumer then reads the raw memory as the wrong layout.

The Vulkan spec says the same from the other side: an import of a dedicated
allocation's payload must itself be dedicated, to an image created
identically to the exporter's.

## Fix

Create the consumer image first, then import its memory with
`VkMemoryDedicatedAllocateInfo { image }` chained beside
`VkImportMemoryFdInfoKHR`, and bind at offset 0:

```rust
let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
    .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
    .fd(fd)
    .build();
let mut dedicated = vk::MemoryDedicatedAllocateInfo::builder().image(image).build();
let alloc_info = vk::MemoryAllocateInfo::builder()
    .allocation_size(exporters_allocation_size)
    .memory_type_index(exporters_memory_type_index)
    .push_next(&mut import_info)
    .push_next(&mut dedicated)
    .build();
```

A CUDA importer of the same memory sets `cudaExternalMemoryDedicated` for the
same reason.

## Where it lives

- `ConsumerVulkanTexture::from_opaque_fd` and
  `ConsumerVulkanDevice::import_opaque_fd_memory_dedicated_to_image_at_stated_memory_type_index`
  in `runtime/streamlib-consumer-rhi/`.
- The hardware test
  `every_opaque_fd_image_of_a_held_pool_reads_back_on_a_consumer_as_the_exporter_wrote_it`
  in `runtime/streamlib-engine/src/vulkan/rhi/vulkan_texture.rs`.
