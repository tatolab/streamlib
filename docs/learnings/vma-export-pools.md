# VMA pool pattern for DMA-BUF exportable allocations

## When you need this

You have a mix of allocations:
- Some need DMA-BUF export (cross-process IPC pixel buffers, shared
  textures)
- Most don't (internal compute outputs, render target textures)

VMA gives you two ways to add `VkExportMemoryAllocateInfo::DMA_BUF_EXT`
to allocations:

| Mechanism | Scope |
|---|---|
| `VmaAllocatorCreateInfo::pTypeExternalMemoryHandleTypes` | Global — affects EVERY allocation |
| `VmaPoolCreateInfo::pMemoryAllocateNext` | Per-pool — affects only allocations from that pool |

**Always use the per-pool mechanism.** Global makes every block
exportable, hitting NVIDIA's allocation cap after swapchain creation
(@docs/learnings/nvidia-dma-buf-after-swapchain.md).

## Pattern: custom VMA pool with `pMemoryAllocateNext`

```rust
// VMA stores a raw pointer to the export info struct internally — must
// outlive the pool. Heap-box for stable address.
let mut export_info = Box::new(
    vk::ExportMemoryAllocateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build(),
);

// Discover the right memory_type_index for the resources you'll allocate
let probe_buffer_info = vk::BufferCreateInfo::builder()
    .size(64 * 1024)  // small probe size
    .usage(vk::BufferUsageFlags::TRANSFER_SRC | ...)
    .sharing_mode(vk::SharingMode::EXCLUSIVE);
let probe_alloc_opts = vma::AllocationOptions {
    flags: vma::AllocationCreateFlags::DEDICATED_MEMORY | ...,
    required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE | ...,
    ..Default::default()
};
let mem_type_idx = unsafe {
    allocator.find_memory_type_index_for_buffer_info(
        probe_buffer_info, &probe_alloc_opts
    )
}?;

// Build the pool
let mut pool_options = vma::PoolOptions::default();
pool_options = pool_options.push_next(export_info.as_mut());
pool_options.memory_type_index = mem_type_idx;
let pool = allocator.create_pool(&pool_options)?;

// All allocations through this pool are DMA-BUF exportable
let (buffer, alloc) = unsafe { pool.create_buffer(buf_info, &alloc_opts) }?;
```

## Drop order (critical)

VMA's pool internally holds the raw pointer to your export info. The Box
must outlive the pool. The allocator must outlive the pool (pool's
internal `Arc<Allocator>` keeps it alive but explicit ordering in `Drop`
is safer):

```rust
impl Drop for MyDevice {
    fn drop(&mut self) {
        // 1. Pools first — vmaDestroyPool frees blocks
        drop(self.dma_buf_pool.take());
        // 2. Allocator — vmaDestroyAllocator
        drop(self.allocator.take());
        // 3. Export info Boxes — VMA no longer references them
        drop(self.export_info.take());
        // 4. Vulkan device + instance
        unsafe {
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
```

## Reference
- Implementation: `libs/streamlib/src/vulkan/rhi/vulkan_device.rs::create_dma_buf_pools`
- Used by: `VulkanPixelBuffer::new()`, `VulkanTexture::new()`
