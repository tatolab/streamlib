// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkCodecUtils/VkBufferResource.h + VkBufferResource.cpp
//!
//! Wraps a `VkBuffer` + VMA allocation with host-visible mapping support,
//! reference counting (via `Arc`), and convenience copy/memset methods.
//!
//! Key Rust divergences from C++:
//! - `VkVideoRefCountBase` reference counting is replaced by `Arc<VkBufferResource>`.
//! - Memory allocation uses `vulkanalia-vma` (VMA) instead of manual
//!   `vkAllocateMemory` + `find_memory_type_index`.
//! - The `VulkanDeviceContext` dispatch table is replaced by `vulkanalia::Device`.
//! - Methods that mutate the buffer take `&mut self`; the C++ relies on interior
//!   mutability via the dispatch table and mapped pointer.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
use std::ptr;
use std::sync::Arc;

/// Alignment helper: round `value` up to the next multiple of `alignment`.
///
/// `alignment` must be a power of two. Returns `value` unchanged when it is
/// already aligned.
#[inline]
pub fn align_up(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
    debug_assert!(alignment > 0 && alignment.is_power_of_two());
    (value + (alignment - 1)) & !(alignment - 1)
}

// ---------------------------------------------------------------------------
// DeviceMemory — VMA-backed helper for mapped data access
// ---------------------------------------------------------------------------

/// VMA-backed device-memory block that tracks an allocation, an optional
/// persistent host-mapped pointer, and provides data access / flush helpers.
///
/// This struct does **not** own the allocation lifecycle — destruction is
/// handled by `VkBufferResource::deinitialize()` via
/// `allocator.destroy_buffer()`.  `DeviceMemory` only provides the data-plane
/// operations (read, write, memset, flush, invalidate).
struct DeviceMemory {
    allocator: Arc<vma::Allocator>,
    allocation: vma::Allocation,
    memory_requirements: vk::MemoryRequirements,
    memory_property_flags: vk::MemoryPropertyFlags,
    /// Persistent mapped pointer (non-null only for host-visible memory).
    mapped_ptr: *mut u8,
}

// Safety: the raw pointer (`mapped_ptr`) is only dereferenced while the
// VMA allocation is live and properly synchronised by the caller.
unsafe impl Send for DeviceMemory {}
unsafe impl Sync for DeviceMemory {}

impl DeviceMemory {
    /// Wrap a VMA allocation with data-access helpers.
    ///
    /// The allocation must already be created and (if host-visible) mapped via
    /// `AllocationCreateFlags::MAPPED`.  `init_data` and `clear_memory` are
    /// applied to the mapped region if available.
    fn new(
        allocator: &Arc<vma::Allocator>,
        allocation: vma::Allocation,
        requirements: vk::MemoryRequirements,
        property_flags: vk::MemoryPropertyFlags,
        init_data: Option<&[u8]>,
        clear_memory: bool,
    ) -> Self {
        // Retrieve the mapped pointer from VMA allocation info.
        let info = allocator.get_allocation_info(allocation);
        let mapped_ptr = if property_flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
            info.pMappedData as *mut u8
        } else {
            ptr::null_mut()
        };

        // Optional initialisation
        if !mapped_ptr.is_null() {
            if let Some(data) = init_data {
                let copy_len = data.len().min(requirements.size as usize);
                unsafe {
                    ptr::copy_nonoverlapping(data.as_ptr(), mapped_ptr, copy_len);
                }
            }
            if clear_memory {
                unsafe {
                    ptr::write_bytes(mapped_ptr, 0u8, requirements.size as usize);
                }
            }
        }

        Self {
            allocator: Arc::clone(allocator),
            allocation,
            memory_requirements: requirements,
            memory_property_flags: property_flags,
            mapped_ptr,
        }
    }

    #[inline]
    fn get_device_memory(&self) -> vk::DeviceMemory {
        self.allocator.get_allocation_info(self.allocation).deviceMemory
    }

    #[inline]
    fn get_memory_requirements(&self) -> &vk::MemoryRequirements {
        &self.memory_requirements
    }

    /// Return a pointer into the mapped region at `offset`, or `None` if the
    /// memory is not host-visible or the access is out of range.
    fn check_access(&self, offset: vk::DeviceSize, size: vk::DeviceSize) -> Option<*mut u8> {
        if self.mapped_ptr.is_null() {
            return None;
        }
        if offset + size > self.memory_requirements.size {
            return None;
        }
        Some(unsafe { self.mapped_ptr.add(offset as usize) })
    }

    fn get_read_only_data_ptr(
        &self,
        offset: vk::DeviceSize,
        max_size: &mut vk::DeviceSize,
    ) -> Option<*const u8> {
        let ptr = self.check_access(offset, 0)?;
        *max_size = self.memory_requirements.size - offset;
        Some(ptr as *const u8)
    }

    fn memset_data(
        &self,
        value: u32,
        offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if let Some(ptr) = self.check_access(offset, size) {
            unsafe {
                ptr::write_bytes(ptr, value as u8, size as usize);
            }
            size as i64
        } else {
            -1
        }
    }

    fn copy_data_to_buffer(
        &self,
        dst_buffer: *mut u8,
        dst_offset: vk::DeviceSize,
        src_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if let Some(src) = self.check_access(src_offset, size) {
            unsafe {
                ptr::copy_nonoverlapping(
                    src as *const u8,
                    dst_buffer.add(dst_offset as usize),
                    size as usize,
                );
            }
            size as i64
        } else {
            -1
        }
    }

    fn copy_data_from_buffer(
        &self,
        source_buffer: *const u8,
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if let Some(dst) = self.check_access(dst_offset, size) {
            unsafe {
                ptr::copy_nonoverlapping(
                    source_buffer.add(src_offset as usize),
                    dst,
                    size as usize,
                );
            }
            size as i64
        } else {
            -1
        }
    }

    fn copy_data_to_memory(
        &self,
        data: &[u8],
        memory_offset: vk::DeviceSize,
    ) -> vk::Result {
        if let Some(dst) = self.check_access(memory_offset, data.len() as vk::DeviceSize) {
            unsafe {
                ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
            }
            vk::Result::SUCCESS
        } else {
            vk::Result::ERROR_MEMORY_MAP_FAILED
        }
    }

    fn flush_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if self
            .memory_property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT)
        {
            return;
        }
        unsafe {
            let _ = self
                .allocator
                .flush_allocation(self.allocation, offset, size);
        }
    }

    fn invalidate_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if self
            .memory_property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT)
        {
            return;
        }
        unsafe {
            let _ = self
                .allocator
                .invalidate_allocation(self.allocation, offset, size);
        }
    }

    /// Return the VMA allocation handle (needed by `deinitialize` for
    /// `destroy_buffer`).
    #[inline]
    fn allocation(&self) -> vma::Allocation {
        self.allocation
    }
}

// DeviceMemory does NOT implement Drop — the allocation is destroyed by
// VkBufferResource::deinitialize() via allocator.destroy_buffer().

// ---------------------------------------------------------------------------
// VkBufferResource
// ---------------------------------------------------------------------------

/// Configuration needed to create and recreate buffers.
///
/// Stored so that `resize` and `clone` can build new buffers with the same
/// parameters.
#[derive(Clone)]
pub struct VkBufferResourceConfig {
    pub device: vulkanalia::Device,
    pub allocator: Arc<vma::Allocator>,
    pub usage: vk::BufferUsageFlags,
    pub memory_property_flags: vk::MemoryPropertyFlags,
    pub buffer_offset_alignment: vk::DeviceSize,
    pub buffer_size_alignment: vk::DeviceSize,
    pub queue_family_indexes: Vec<u32>,
}

/// Vulkan buffer resource with automatic memory management via VMA.
///
/// Port of `VkBufferResource` — wraps a `VkBuffer` + VMA allocation,
/// supports host-visible mapping, and provides copy / memset helpers.
///
/// Use [`VkBufferResource::create`] to construct; the type is typically held
/// behind an `Arc` for shared ownership (replacing the C++ reference counting).
pub struct VkBufferResource {
    config: VkBufferResourceConfig,
    buffer: vk::Buffer,
    buffer_offset: vk::DeviceSize,
    buffer_size: vk::DeviceSize,
    device_memory: Option<DeviceMemory>,
}

impl VkBufferResource {
    // -- Construction -------------------------------------------------------

    /// Create a Vulkan buffer with VMA-managed device memory.
    ///
    /// This is the Rust equivalent of the C++ static `Create` factory.
    pub fn create(
        device: vulkanalia::Device,
        allocator: Arc<vma::Allocator>,
        usage: vk::BufferUsageFlags,
        memory_property_flags: vk::MemoryPropertyFlags,
        buffer_size: vk::DeviceSize,
        buffer_offset_alignment: vk::DeviceSize,
        buffer_size_alignment: vk::DeviceSize,
        init_data: Option<&[u8]>,
        queue_family_indexes: &[u32],
    ) -> Result<Arc<Self>, vk::Result> {
        let config = VkBufferResourceConfig {
            device,
            allocator,
            usage,
            memory_property_flags,
            buffer_offset_alignment: if buffer_offset_alignment == 0 {
                1
            } else {
                buffer_offset_alignment
            },
            buffer_size_alignment: if buffer_size_alignment == 0 {
                1
            } else {
                buffer_size_alignment
            },
            queue_family_indexes: queue_family_indexes.to_vec(),
        };

        let mut resource = Self {
            config,
            buffer: vk::Buffer::null(),
            buffer_offset: 0,
            buffer_size: 0,
            device_memory: None,
        };

        resource.initialize(buffer_size, init_data)?;
        Ok(Arc::new(resource))
    }

    // -- Accessors ----------------------------------------------------------

    /// Get maximum buffer size in bytes.
    #[inline]
    pub fn get_max_size(&self) -> vk::DeviceSize {
        self.buffer_size
    }

    /// Get buffer offset alignment requirement.
    #[inline]
    pub fn get_offset_alignment(&self) -> vk::DeviceSize {
        self.config.buffer_offset_alignment
    }

    /// Get buffer size alignment (from memory requirements).
    #[inline]
    pub fn get_size_alignment(&self) -> vk::DeviceSize {
        match &self.device_memory {
            Some(mem) => mem.get_memory_requirements().alignment,
            None => 1,
        }
    }

    /// Get underlying `VkBuffer` handle.
    #[inline]
    pub fn get_buffer(&self) -> vk::Buffer {
        self.buffer
    }

    /// Get underlying `VkDeviceMemory` handle.
    #[inline]
    pub fn get_device_memory(&self) -> vk::DeviceMemory {
        match &self.device_memory {
            Some(mem) => mem.get_device_memory(),
            None => vk::DeviceMemory::null(),
        }
    }

    /// Check if the buffer is valid (non-null handle).
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.buffer != vk::Buffer::null()
    }

    // -- Data access --------------------------------------------------------

    /// Get a writable pointer to buffer data (host-visible memory only).
    ///
    /// Returns `(pointer, remaining_bytes)` or `None` if not host-visible.
    pub fn get_data_ptr(&self, offset: vk::DeviceSize) -> Option<(*mut u8, vk::DeviceSize)> {
        let ptr = self.check_access(offset, 1)?;
        let max_size = self.buffer_size - offset;
        Some((ptr, max_size))
    }

    /// Get a read-only pointer to buffer data (host-visible memory only).
    ///
    /// Returns `(pointer, remaining_bytes)` or `None` if not host-visible.
    pub fn get_read_only_data_ptr(
        &self,
        offset: vk::DeviceSize,
    ) -> Option<(*const u8, vk::DeviceSize)> {
        let ptr = self.check_access(offset, 1)?;
        let max_size = self.buffer_size - offset;
        Some((ptr as *const u8, max_size))
    }

    // -- Copy / memset operations -------------------------------------------

    /// Fill buffer region with `value` (CPU memset).
    pub fn memset_data(
        &self,
        value: u32,
        offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        match &self.device_memory {
            Some(mem) => mem.memset_data(value, self.buffer_offset + offset, size),
            None => -1,
        }
    }

    /// Copy data from this buffer to a CPU byte slice.
    pub fn copy_data_to_slice(
        &self,
        dst_buffer: &mut [u8],
        dst_offset: vk::DeviceSize,
        src_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        match &self.device_memory {
            Some(mem) => mem.copy_data_to_buffer(
                dst_buffer.as_mut_ptr(),
                dst_offset,
                self.buffer_offset + src_offset,
                size,
            ),
            None => -1,
        }
    }

    /// Copy data from this buffer to another `VkBufferResource`.
    pub fn copy_data_to_buffer(
        &self,
        dst_buffer: &VkBufferResource,
        dst_offset: vk::DeviceSize,
        src_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        let read_data = match self.check_access(src_offset, size) {
            Some(ptr) => ptr,
            None => return -1,
        };
        dst_buffer.copy_data_from_ptr(
            read_data as *const u8,
            0,
            self.buffer_offset + dst_offset,
            size,
        )
    }

    /// Copy data from a CPU byte slice into this buffer.
    pub fn copy_data_from_slice(
        &self,
        source: &[u8],
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        match &self.device_memory {
            Some(mem) => mem.copy_data_from_buffer(
                source.as_ptr(),
                src_offset,
                self.buffer_offset + dst_offset,
                size,
            ),
            None => -1,
        }
    }

    /// Copy data from another `VkBufferResource` into this buffer.
    pub fn copy_data_from_buffer(
        &self,
        source_buffer: &VkBufferResource,
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        let (read_data, _max) = match source_buffer.get_read_only_data_ptr(src_offset) {
            Some(v) => v,
            None => return -1,
        };
        match &self.device_memory {
            Some(mem) => {
                mem.copy_data_from_buffer(read_data, 0, self.buffer_offset + dst_offset, size)
            }
            None => -1,
        }
    }

    /// Copy data into the buffer at an aligned offset, advancing
    /// `dst_buffer_offset`.
    ///
    /// This is the Rust equivalent of the C++ overload
    /// `CopyDataToBuffer(pData, size, dstBufferOffset)`.
    pub fn copy_data_to_buffer_aligned(
        &self,
        data: &[u8],
        dst_buffer_offset: &mut vk::DeviceSize,
    ) -> vk::Result {
        if data.is_empty() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }

        *dst_buffer_offset = align_up(*dst_buffer_offset, self.config.buffer_offset_alignment);
        debug_assert!(*dst_buffer_offset + data.len() as vk::DeviceSize <= self.buffer_size);

        match &self.device_memory {
            Some(mem) => mem.copy_data_to_memory(data, self.buffer_offset + *dst_buffer_offset),
            None => vk::Result::ERROR_MEMORY_MAP_FAILED,
        }
    }

    // -- Flush / Invalidate -------------------------------------------------

    /// Flush CPU writes to GPU (for non-coherent memory).
    pub fn flush_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if size == 0 {
            return;
        }
        if let Some(mem) = &self.device_memory {
            mem.flush_range(offset, size);
        }
    }

    /// Invalidate GPU writes to CPU (for non-coherent memory).
    pub fn invalidate_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if size == 0 {
            return;
        }
        if let Some(mem) = &self.device_memory {
            mem.invalidate_range(offset, size);
        }
    }

    // -- Resize / Clone -----------------------------------------------------

    /// Resize the buffer in-place, optionally copying existing data.
    ///
    /// Returns the new size, or 0 on failure.
    ///
    /// # Safety
    /// This destroys the previous buffer and memory.  No outstanding GPU
    /// operations may reference the old handles.
    pub unsafe fn resize(
        &mut self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> vk::DeviceSize {
        if self.buffer_size >= new_size {
            return self.buffer_size;
        }

        // Read old data if needed
        let init_data: Option<Vec<u8>> = if copy_size > 0 {
            if let Some(mem) = &self.device_memory {
                let mut max_size: vk::DeviceSize = 0;
                if let Some(ptr) = mem.get_read_only_data_ptr(copy_offset, &mut max_size) {
                    debug_assert!(copy_size <= max_size);
                    let mut buf = vec![0u8; copy_size as usize];
                    ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), copy_size as usize);
                    Some(buf)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let (new_buffer, new_offset, new_device_memory, aligned_size) = match Self::create_buffer(
            &self.config,
            new_size,
            init_data.as_deref(),
        ) {
            Ok(v) => v,
            Err(_) => return 0,
        };

        self.deinitialize();

        self.buffer = new_buffer;
        self.device_memory = Some(new_device_memory);
        self.buffer_offset = new_offset;
        self.buffer_size = aligned_size;

        aligned_size
    }

    /// Clone this buffer's configuration into a new `VkBufferResource`,
    /// optionally copying data.
    ///
    /// Returns `(new_resource, new_size)` or an error.
    pub fn clone_buffer(
        &self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> Result<(Arc<VkBufferResource>, vk::DeviceSize), vk::Result> {
        let init_data: Option<Vec<u8>> = if copy_size > 0 {
            if let Some((ptr, _max)) = self.get_data_ptr(copy_offset) {
                let mut buf = vec![0u8; copy_size as usize];
                unsafe {
                    ptr::copy_nonoverlapping(ptr as *const u8, buf.as_mut_ptr(), copy_size as usize);
                }
                Some(buf)
            } else {
                None
            }
        } else {
            None
        };

        let (new_buffer, new_offset, new_device_memory, aligned_size) =
            Self::create_buffer(&self.config, new_size, init_data.as_deref())?;

        let resource = Arc::new(VkBufferResource {
            config: self.config.clone(),
            buffer: new_buffer,
            buffer_offset: new_offset,
            buffer_size: aligned_size,
            device_memory: Some(new_device_memory),
        });

        Ok((resource, aligned_size))
    }

    // -- Private helpers ----------------------------------------------------

    /// Low-level raw pointer copy from buffer — used by `copy_data_to_buffer`
    /// (buffer-to-buffer variant).
    fn copy_data_from_ptr(
        &self,
        source: *const u8,
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        match &self.device_memory {
            Some(mem) => mem.copy_data_from_buffer(source, src_offset, dst_offset, size),
            None => -1,
        }
    }

    fn check_access(&self, offset: vk::DeviceSize, size: vk::DeviceSize) -> Option<*mut u8> {
        if offset + size > self.buffer_size {
            return None;
        }
        let mem = self.device_memory.as_ref()?;
        let base = mem.check_access(self.buffer_offset, size)?;
        Some(unsafe { base.add(offset as usize) })
    }

    fn initialize(
        &mut self,
        buffer_size: vk::DeviceSize,
        init_data: Option<&[u8]>,
    ) -> Result<(), vk::Result> {
        if self.buffer_size >= buffer_size {
            // Already large enough — optionally clear.
            #[cfg(feature = "clear_bitstream_buffers_on_create")]
            {
                let ret = self.memset_data(0x00, 0, self.buffer_size);
                if ret != self.buffer_size as i64 {
                    return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
                }
            }
            return Ok(());
        }

        self.deinitialize();

        let (new_buffer, new_offset, new_device_memory, aligned_size) =
            Self::create_buffer(&self.config, buffer_size, init_data)?;

        self.buffer = new_buffer;
        self.buffer_offset = new_offset;
        self.buffer_size = aligned_size;
        self.device_memory = Some(new_device_memory);

        Ok(())
    }

    fn deinitialize(&mut self) {
        if self.buffer != vk::Buffer::null() {
            if let Some(mem) = self.device_memory.take() {
                unsafe {
                    self.config
                        .allocator
                        .destroy_buffer(self.buffer, mem.allocation());
                }
            } else {
                // Buffer without allocation — should not happen, but clean up.
                unsafe {
                    self.config.device.destroy_buffer(self.buffer, None);
                }
            }
            self.buffer = vk::Buffer::null();
        } else {
            // No buffer — just drop the DeviceMemory (no-op since it has no Drop).
            self.device_memory = None;
        }
        self.buffer_offset = 0;
        self.buffer_size = 0;
    }

    /// Create a `VkBuffer` + VMA allocation in one call.
    ///
    /// Returns `(buffer, buffer_offset, device_memory, aligned_buffer_size)`.
    fn create_buffer(
        config: &VkBufferResourceConfig,
        buffer_size: vk::DeviceSize,
        init_data: Option<&[u8]>,
    ) -> Result<(vk::Buffer, vk::DeviceSize, DeviceMemory, vk::DeviceSize), vk::Result> {
        let aligned_size = align_up(buffer_size, config.buffer_size_alignment);
        let buffer_offset: vk::DeviceSize = 0;

        let mut create_info = vk::BufferCreateInfo::builder()
            .size(aligned_size)
            .usage(config.usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        if !config.queue_family_indexes.is_empty() {
            create_info = create_info.queue_family_indices(&config.queue_family_indexes);
        }

        // Build VMA allocation options.  For host-visible memory we request
        // MAPPED so VMA persistently maps it and provides pMappedData.
        let is_host_visible = config
            .memory_property_flags
            .contains(vk::MemoryPropertyFlags::HOST_VISIBLE);

        let mut alloc_flags = vma::AllocationCreateFlags::empty();
        if is_host_visible {
            alloc_flags |=
                vma::AllocationCreateFlags::MAPPED | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE;
        }

        let alloc_opts = vma::AllocationOptions {
            flags: alloc_flags,
            usage: vma::MemoryUsage::Auto,
            required_flags: config.memory_property_flags,
            ..Default::default()
        };

        let (buffer, allocation) = unsafe {
            config
                .allocator
                .create_buffer(create_info, &alloc_opts)
                .map_err(|e| vk::Result::from(e))?
        };

        // Retrieve the actual memory property flags from the chosen memory
        // type so that flush/invalidate can check for HOST_COHERENT.
        let alloc_info = config.allocator.get_allocation_info(allocation);
        let mem_props = config.allocator.get_memory_properties();
        let actual_flags = mem_props.memory_types[alloc_info.memoryType as usize].property_flags;

        // Build memory requirements from the allocation info.  VMA knows the
        // true size/alignment, but for the data-access helpers we need a
        // MemoryRequirements struct.  We reconstruct it from the allocation.
        let requirements = vk::MemoryRequirements {
            size: alloc_info.size,
            alignment: config.buffer_size_alignment.max(1),
            memory_type_bits: 1 << alloc_info.memoryType,
        };

        let device_memory = DeviceMemory::new(
            &config.allocator,
            allocation,
            requirements,
            actual_flags,
            init_data,
            cfg!(feature = "clear_bitstream_buffers_on_create"),
        );

        Ok((buffer, buffer_offset, device_memory, aligned_size))
    }
}

impl Drop for VkBufferResource {
    fn drop(&mut self) {
        self.deinitialize();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up_basic() {
        assert_eq!(align_up(0, 16), 0);
        assert_eq!(align_up(1, 16), 16);
        assert_eq!(align_up(15, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn test_align_up_alignment_one() {
        // Alignment of 1 should be a no-op.
        for v in [0, 1, 7, 255, 1024] {
            assert_eq!(align_up(v, 1), v);
        }
    }

    #[test]
    fn test_align_up_powers_of_two() {
        assert_eq!(align_up(100, 64), 128);
        assert_eq!(align_up(128, 64), 128);
        assert_eq!(align_up(129, 64), 192);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(1, 4096), 4096);
    }

    #[test]
    fn test_align_up_large_values() {
        let gb = 1u64 << 30;
        assert_eq!(align_up(gb + 1, 4096), gb + 4096);
    }
}
