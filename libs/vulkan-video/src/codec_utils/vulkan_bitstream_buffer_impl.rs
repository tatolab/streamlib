// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VulkanBitstreamBuffer.h (trait) + VulkanBistreamBufferImpl.h/.cpp (impl).
//!
//! Implements a Vulkan-backed bitstream buffer for holding compressed video data
//! (encode or decode). The buffer is backed by host-visible device memory so the
//! CPU can read/write the bitstream while the GPU can consume/produce it.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
use std::ptr;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// VulkanBitstreamBuffer — trait (port of the C++ pure-virtual base class)
// ---------------------------------------------------------------------------

/// Trait corresponding to the C++ `VulkanBitstreamBuffer` abstract class.
///
/// Provides the interface for a Vulkan-backed bitstream buffer used by the
/// video decode/encode pipelines.
pub trait VulkanBitstreamBuffer {
    fn get_max_size(&self) -> vk::DeviceSize;
    fn get_offset_alignment(&self) -> vk::DeviceSize;
    fn get_size_alignment(&self) -> vk::DeviceSize;

    /// Resize the buffer in-place. Returns the new (or existing) size.
    /// If `copy_size > 0`, copies that many bytes from `copy_offset` in the old
    /// buffer into the new one.
    fn resize(
        &mut self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> vk::DeviceSize;

    /// Clone the buffer into a new `VulkanBitstreamBufferImpl`, optionally
    /// copying data from the current buffer.  Returns the new size on success.
    fn clone_buffer(
        &mut self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> Result<Arc<VulkanBitstreamBufferImpl>, vk::Result>;

    fn memset_data(&mut self, value: u32, offset: vk::DeviceSize, size: vk::DeviceSize) -> i64;

    /// Copy from this buffer into a caller-supplied byte slice.
    fn copy_data_to_slice(
        &self,
        dst: &mut [u8],
        dst_offset: vk::DeviceSize,
        src_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64;

    /// Copy from a caller-supplied byte slice into this buffer.
    fn copy_data_from_slice(
        &mut self,
        src: &[u8],
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64;

    /// Get a mutable pointer to the mapped buffer data at `offset`.
    /// On success sets `max_size` to the remaining bytes from `offset`.
    fn get_data_ptr(&mut self, offset: vk::DeviceSize) -> Option<(*mut u8, vk::DeviceSize)>;

    /// Get a read-only pointer to the mapped buffer data at `offset`.
    /// On success sets `max_size` to the remaining bytes from `offset`.
    fn get_read_only_data_ptr(
        &self,
        offset: vk::DeviceSize,
    ) -> Option<(*const u8, vk::DeviceSize)>;

    fn flush_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize);
    fn invalidate_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize);

    fn get_buffer(&self) -> vk::Buffer;
    fn get_device_memory(&self) -> vk::DeviceMemory;

    // Stream marker API — tracks slice offsets within the bitstream.
    fn add_stream_marker(&mut self, stream_offset: u32) -> u32;
    fn set_stream_marker(&mut self, stream_offset: u32, index: u32) -> u32;
    fn get_stream_marker(&self, index: u32) -> u32;
    fn get_stream_markers_count(&self) -> u32;
    fn get_stream_markers(&self, start_index: u32) -> &[u32];
    fn reset_stream_markers(&mut self) -> u32;
}

// ---------------------------------------------------------------------------
// VulkanBitstreamBufferImpl — the concrete implementation
// ---------------------------------------------------------------------------

/// Port of C++ `VulkanBitstreamBufferImpl`.
///
/// Owns a `vk::Buffer` + device memory allocation. The memory is persistently
/// mapped (host-visible + host-coherent) so callers can read/write the
/// bitstream directly via pointer.
pub struct VulkanBitstreamBufferImpl {
    /// VMA allocator — shared across all buffers that use the same device.
    allocator: Arc<vma::Allocator>,
    queue_family_index: u32,
    memory_property_flags: vk::MemoryPropertyFlags,
    buffer: vk::Buffer,
    buffer_offset: vk::DeviceSize,
    buffer_size: vk::DeviceSize,
    buffer_offset_alignment: vk::DeviceSize,
    buffer_size_alignment: vk::DeviceSize,
    /// VMA allocation handle (replaces manual DeviceMemory + size tracking).
    allocation: vma::Allocation,
    /// Persistently mapped pointer (set once after allocation, remains valid
    /// until the memory is freed).
    device_memory_ptr: *mut u8,
    stream_markers: Vec<u32>,
    usage: vk::BufferUsageFlags,
}

// SAFETY: The raw pointer `device_memory_ptr` points to a persistently mapped
// Vulkan allocation whose lifetime is tied to `self`.  We do not hand out
// references that outlive `self`, so Send/Sync is safe as long as the caller
// upholds normal Vulkan external-synchronisation rules (which is the same
// contract the C++ code has).
unsafe impl Send for VulkanBitstreamBufferImpl {}
unsafe impl Sync for VulkanBitstreamBufferImpl {}

impl VulkanBitstreamBufferImpl {
    /// Factory — port of the static `VulkanBitstreamBufferImpl::Create`.
    ///
    /// Creates a Vulkan buffer of `buffer_size` bytes backed by host-visible
    /// memory.  Optionally copies `initialize_data` into the buffer.
    pub fn create(
        allocator: Arc<vma::Allocator>,
        queue_family_index: u32,
        usage: vk::BufferUsageFlags,
        buffer_size: vk::DeviceSize,
        buffer_offset_alignment: vk::DeviceSize,
        buffer_size_alignment: vk::DeviceSize,
        initialize_data: Option<&[u8]>,
    ) -> Result<Arc<Self>, vk::Result> {
        let mut this = Self {
            allocator,
            queue_family_index,
            memory_property_flags: vk::MemoryPropertyFlags::empty(),
            buffer: vk::Buffer::null(),
            buffer_offset: 0,
            buffer_size: 0,
            buffer_offset_alignment,
            buffer_size_alignment,
            allocation: unsafe { std::mem::zeroed() },
            device_memory_ptr: ptr::null_mut(),
            stream_markers: Vec::with_capacity(256),
            usage,
        };

        this.initialize(buffer_size, initialize_data)?;
        Ok(Arc::new(this))
    }

    // -- private helpers ---------------------------------------------------

    /// Port of `VulkanBitstreamBufferImpl::Initialize`.
    fn initialize(
        &mut self,
        buffer_size: vk::DeviceSize,
        initialize_data: Option<&[u8]>,
    ) -> Result<(), vk::Result> {
        // If the current allocation is already large enough, skip re-creation.
        if self.buffer_size >= buffer_size {
            return Ok(());
        }

        self.deinitialize();

        self.memory_property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT
            | vk::MemoryPropertyFlags::HOST_CACHED;

        self.create_buffer(buffer_size, initialize_data)?;

        Ok(())
    }

    /// Port of `VulkanBitstreamBufferImpl::CreateBuffer`.
    ///
    /// Creates the `vk::Buffer`, allocates + binds device memory, maps it, and
    /// optionally copies initialisation data.  Uses VMA to handle memory type
    /// selection (with HOST_CACHED as preferred, falling back automatically),
    /// allocation, binding, and persistent mapping in a single call.
    fn create_buffer(
        &mut self,
        buffer_size: vk::DeviceSize,
        initialize_data: Option<&[u8]>,
    ) -> Result<(), vk::Result> {
        // Align buffer size up to `buffer_size_alignment`.
        let aligned_size = align_up(buffer_size, self.buffer_size_alignment);

        let create_info = vk::BufferCreateInfo::builder()
            .size(aligned_size)
            .usage(self.usage)
            .flags(vk::BufferCreateFlags::VIDEO_PROFILE_INDEPENDENT_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .queue_family_indices(std::slice::from_ref(&self.queue_family_index));

        let alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            preferred_flags: vk::MemoryPropertyFlags::HOST_CACHED,
            ..Default::default()
        };

        // VMA creates the buffer, finds a suitable memory type (preferring
        // HOST_CACHED, falling back automatically), allocates, binds, and maps
        // — all in one call.
        let (buffer, allocation) = unsafe {
            self.allocator
                .create_buffer(create_info, &alloc_options)?
        };

        // Retrieve the persistently mapped pointer from the allocation info.
        let info = self.allocator.get_allocation_info(allocation);
        let mapped_ptr = info.pMappedData as *mut u8;
        if mapped_ptr.is_null() {
            // Should not happen with MAPPED flag, but guard defensively.
            unsafe { self.allocator.destroy_buffer(buffer, allocation) };
            return Err(vk::Result::ERROR_MEMORY_MAP_FAILED);
        }

        // Record the actual memory property flags for coherency checks.
        let mem_props = self.allocator.get_memory_properties();
        self.memory_property_flags =
            mem_props.memory_types[info.memoryType as usize].property_flags;

        // Optionally copy initialization data.
        if let Some(data) = initialize_data {
            let copy_len = data.len().min(aligned_size as usize);
            unsafe {
                ptr::copy_nonoverlapping(data.as_ptr(), mapped_ptr, copy_len);
            }
        }

        self.buffer = buffer;
        self.buffer_offset = 0;
        self.buffer_size = aligned_size;
        self.allocation = allocation;
        self.device_memory_ptr = mapped_ptr;

        Ok(())
    }

    /// Port of `VulkanBitstreamBufferImpl::Deinitialize`.
    fn deinitialize(&mut self) {
        if self.buffer != vk::Buffer::null() {
            // VMA's destroy_buffer handles unmapping, freeing memory, and
            // destroying the buffer in one call.
            unsafe {
                self.allocator.destroy_buffer(self.buffer, self.allocation);
            }
            self.buffer = vk::Buffer::null();
            self.allocation = unsafe { std::mem::zeroed() };
        }
        self.device_memory_ptr = ptr::null_mut();
        self.buffer_offset = 0;
        self.buffer_size = 0;
    }

    /// Port of `VulkanBitstreamBufferImpl::CheckAccess`.
    ///
    /// Returns a pointer into the mapped memory at the given offset, or `None`
    /// if the access would be out of range or the memory is not mapped.
    fn check_access(&self, offset: vk::DeviceSize, size: vk::DeviceSize) -> Option<*mut u8> {
        if offset + size > self.buffer_size {
            return None;
        }
        if self.device_memory_ptr.is_null() {
            return None;
        }
        // The mapped pointer covers the whole allocation starting at 0.
        // `buffer_offset` is the offset of our buffer within that allocation.
        // We add the caller's logical offset on top.
        Some(unsafe { self.device_memory_ptr.add((self.buffer_offset + offset) as usize) })
    }

    /// Port of the private `CopyDataToBuffer(pData, size, &dstBufferOffset)`
    /// overload used by the C++ code.  Aligns `dst_buffer_offset` and copies
    /// `data` into the mapped memory.
    pub fn copy_data_to_buffer_aligned(
        &self,
        data: &[u8],
        dst_buffer_offset: &mut vk::DeviceSize,
    ) -> Result<(), vk::Result> {
        if data.is_empty() {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }

        *dst_buffer_offset = align_up(*dst_buffer_offset, self.buffer_offset_alignment);
        debug_assert!(*dst_buffer_offset + data.len() as vk::DeviceSize <= self.buffer_size);

        let dst = self.check_access(*dst_buffer_offset, data.len() as vk::DeviceSize);
        match dst {
            Some(ptr) => {
                unsafe {
                    ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
                }
                Ok(())
            }
            None => Err(vk::Result::ERROR_INITIALIZATION_FAILED),
        }
    }
}

impl VulkanBitstreamBuffer for VulkanBitstreamBufferImpl {
    fn get_max_size(&self) -> vk::DeviceSize {
        self.buffer_size
    }

    fn get_offset_alignment(&self) -> vk::DeviceSize {
        self.buffer_offset_alignment
    }

    fn get_size_alignment(&self) -> vk::DeviceSize {
        self.buffer_size_alignment
    }

    fn resize(
        &mut self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> vk::DeviceSize {
        if self.buffer_size >= new_size {
            return self.buffer_size;
        }

        // Gather data to copy from the old buffer before we tear it down.
        let copy_data: Option<Vec<u8>> = if copy_size > 0 {
            self.check_access(copy_offset, copy_size).map(|ptr| {
                let mut v = vec![0u8; copy_size as usize];
                unsafe {
                    ptr::copy_nonoverlapping(ptr as *const u8, v.as_mut_ptr(), copy_size as usize);
                }
                v
            })
        } else {
            None
        };

        self.deinitialize();

        self.memory_property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT
            | vk::MemoryPropertyFlags::HOST_CACHED;

        let init = copy_data.as_deref();
        match self.create_buffer(new_size, init) {
            Ok(()) => self.buffer_size,
            Err(_) => 0,
        }
    }

    fn clone_buffer(
        &mut self,
        new_size: vk::DeviceSize,
        copy_size: vk::DeviceSize,
        copy_offset: vk::DeviceSize,
    ) -> Result<Arc<VulkanBitstreamBufferImpl>, vk::Result> {
        let init_data: Option<Vec<u8>> = if copy_size > 0 {
            self.check_access(copy_offset, copy_size).map(|ptr| {
                let mut v = vec![0u8; copy_size as usize];
                unsafe {
                    ptr::copy_nonoverlapping(ptr as *const u8, v.as_mut_ptr(), copy_size as usize);
                }
                v
            })
        } else {
            None
        };

        VulkanBitstreamBufferImpl::create(
            Arc::clone(&self.allocator),
            self.queue_family_index,
            self.usage,
            new_size,
            self.buffer_offset_alignment,
            self.buffer_size_alignment,
            init_data.as_deref(),
        )
    }

    fn memset_data(&mut self, value: u32, offset: vk::DeviceSize, size: vk::DeviceSize) -> i64 {
        if size == 0 {
            return 0;
        }
        match self.check_access(offset, size) {
            Some(ptr) => {
                // C++ memset uses the low byte of value.
                let byte = value as u8;
                unsafe {
                    ptr::write_bytes(ptr, byte, size as usize);
                }
                size as i64
            }
            None => -1,
        }
    }

    fn copy_data_to_slice(
        &self,
        dst: &mut [u8],
        dst_offset: vk::DeviceSize,
        src_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        match self.check_access(src_offset, size) {
            Some(src_ptr) => {
                let count = size as usize;
                let dst_start = dst_offset as usize;
                if dst_start + count > dst.len() {
                    return -1;
                }
                unsafe {
                    ptr::copy_nonoverlapping(src_ptr as *const u8, dst[dst_start..].as_mut_ptr(), count);
                }
                size as i64
            }
            None => -1,
        }
    }

    fn copy_data_from_slice(
        &mut self,
        src: &[u8],
        src_offset: vk::DeviceSize,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize,
    ) -> i64 {
        if size == 0 {
            return 0;
        }
        let src_start = src_offset as usize;
        let count = size as usize;
        if src_start + count > src.len() {
            return -1;
        }
        match self.check_access(dst_offset, size) {
            Some(dst_ptr) => {
                unsafe {
                    ptr::copy_nonoverlapping(src[src_start..].as_ptr(), dst_ptr, count);
                }
                size as i64
            }
            None => -1,
        }
    }

    fn get_data_ptr(&mut self, offset: vk::DeviceSize) -> Option<(*mut u8, vk::DeviceSize)> {
        self.check_access(offset, 1).map(|ptr| {
            let max_size = self.buffer_size - offset;
            (ptr, max_size)
        })
    }

    fn get_read_only_data_ptr(
        &self,
        offset: vk::DeviceSize,
    ) -> Option<(*const u8, vk::DeviceSize)> {
        self.check_access(offset, 1).map(|ptr| {
            let max_size = self.buffer_size - offset;
            (ptr as *const u8, max_size)
        })
    }

    fn flush_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if size == 0 {
            return;
        }
        // Skip flush for coherent memory — writes are already visible.
        if self
            .memory_property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT)
        {
            return;
        }
        unsafe {
            let _ = self.allocator.flush_allocation(
                self.allocation,
                self.buffer_offset + offset,
                size,
            );
        }
    }

    fn invalidate_range(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        if size == 0 {
            return;
        }
        // Skip invalidate for coherent memory — GPU writes are already visible.
        if self
            .memory_property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT)
        {
            return;
        }
        unsafe {
            let _ = self.allocator.invalidate_allocation(
                self.allocation,
                self.buffer_offset + offset,
                size,
            );
        }
    }

    fn get_buffer(&self) -> vk::Buffer {
        self.buffer
    }

    fn get_device_memory(&self) -> vk::DeviceMemory {
        let info = self.allocator.get_allocation_info(self.allocation);
        info.deviceMemory
    }

    // -- stream marker methods ---------------------------------------------

    fn add_stream_marker(&mut self, stream_offset: u32) -> u32 {
        self.stream_markers.push(stream_offset);
        (self.stream_markers.len() - 1) as u32
    }

    fn set_stream_marker(&mut self, stream_offset: u32, index: u32) -> u32 {
        let idx = index as usize;
        if idx >= self.stream_markers.len() {
            return u32::MAX;
        }
        self.stream_markers[idx] = stream_offset;
        index
    }

    fn get_stream_marker(&self, index: u32) -> u32 {
        self.stream_markers[index as usize]
    }

    fn get_stream_markers_count(&self) -> u32 {
        self.stream_markers.len() as u32
    }

    fn get_stream_markers(&self, start_index: u32) -> &[u32] {
        &self.stream_markers[start_index as usize..]
    }

    fn reset_stream_markers(&mut self) -> u32 {
        let old_size = self.stream_markers.len() as u32;
        self.stream_markers.clear();
        old_size
    }
}

impl Drop for VulkanBitstreamBufferImpl {
    fn drop(&mut self) {
        self.deinitialize();
    }
}

// ---------------------------------------------------------------------------
// VulkanBitstreamBufferStream — convenience wrapper
// ---------------------------------------------------------------------------

/// Port of C++ `VulkanBitstreamBufferStream`.
///
/// Wraps a `VulkanBitstreamBufferImpl` and caches its mapped data pointer for
/// efficient indexed access.  Tracks the highest written offset so
/// `commit_buffer` can flush only the touched range.
pub struct VulkanBitstreamBufferStream {
    bitstream_buffer: Option<Arc<VulkanBitstreamBufferImpl>>,
    /// Cached data pointer into the mapped buffer.
    data_ptr: *mut u8,
    max_size: vk::DeviceSize,
    max_access_location: vk::DeviceSize,
    num_slices: u32,
}

// SAFETY: Same reasoning as VulkanBitstreamBufferImpl — the pointer is derived
// from a Vulkan mapped allocation owned by the Arc'd buffer.
unsafe impl Send for VulkanBitstreamBufferStream {}
unsafe impl Sync for VulkanBitstreamBufferStream {}

impl VulkanBitstreamBufferStream {
    pub fn new() -> Self {
        Self {
            bitstream_buffer: None,
            data_ptr: ptr::null_mut(),
            max_size: 0,
            max_access_location: 0,
            num_slices: 0,
        }
    }

    /// Flush the written range to the device.
    pub fn commit_buffer(&mut self, size: vk::DeviceSize) -> vk::DeviceSize {
        let commit_size = if size != 0 {
            size
        } else {
            self.max_access_location
        };
        if commit_size > 0 {
            if let Some(ref buf) = self.bitstream_buffer {
                buf.flush_range(0, commit_size);
                self.max_access_location = 0;
            }
        }
        commit_size
    }

    /// Attach a bitstream buffer.  Returns the max writable size.
    pub fn set_bitstream_buffer(
        &mut self,
        buffer: Arc<VulkanBitstreamBufferImpl>,
        reset_stream_markers: bool,
    ) -> vk::DeviceSize {
        self.commit_buffer(0);

        self.max_access_location = 0;

        // We need mutable access to call get_data_ptr. Since we hold the only
        // Arc at setup time we use Arc::get_mut; otherwise fall back to
        // the read-only pointer.
        //
        // Divergence from C++: the C++ code freely calls GetDataPtr on a
        // shared-pointer.  In Rust we use get_read_only_data_ptr since the
        // buffer may be shared.  The pointer is still to mapped memory that is
        // writable at the hardware level.
        let (ptr, max) = buffer
            .get_read_only_data_ptr(0)
            .expect("bitstream buffer must be mapped");
        self.data_ptr = ptr as *mut u8;
        self.max_size = max;

        if reset_stream_markers {
            // Need mutable access — take a clone, set markers via trait.
            // Since markers are on the Arc'd buffer, we use interior approach:
            // store the buffer first, then reset through it.
            self.bitstream_buffer = Some(buffer);
            self.reset_stream_markers();
        } else {
            self.num_slices = buffer.get_stream_markers_count();
            self.bitstream_buffer = Some(buffer);
        }

        self.max_size
    }

    pub fn reset_bitstream_buffer(&mut self) {
        self.commit_buffer(0);
        self.bitstream_buffer = None;
        self.max_access_location = 0;
        self.data_ptr = ptr::null_mut();
    }

    /// Read a byte at the given index.
    ///
    /// # Safety
    /// Caller must ensure `index < max_size`.
    pub unsafe fn read(&self, index: vk::DeviceSize) -> u8 {
        debug_assert!(!self.data_ptr.is_null());
        debug_assert!(index < self.max_size);
        *self.data_ptr.add(index as usize)
    }

    /// Write a byte at the given index, updating the high-water mark.
    ///
    /// # Safety
    /// Caller must ensure `index < max_size`.
    pub unsafe fn write(&mut self, index: vk::DeviceSize, value: u8) {
        debug_assert!(!self.data_ptr.is_null());
        debug_assert!(index < self.max_size);
        if index > self.max_access_location {
            self.max_access_location = index;
        }
        *self.data_ptr.add(index as usize) = value;
    }

    pub fn is_valid(&self) -> bool {
        !self.data_ptr.is_null() && self.max_size != 0 && self.bitstream_buffer.is_some()
    }

    pub fn get_bitstream_buffer(&self) -> Option<&Arc<VulkanBitstreamBufferImpl>> {
        self.bitstream_buffer.as_ref()
    }

    /// Check if a start-code (0x00, 0x00, 0x01) exists at the given offset.
    pub fn has_slice_start_code_at_offset(&self, index: vk::DeviceSize) -> bool {
        debug_assert!(!self.data_ptr.is_null());
        debug_assert!(index + 2 < self.max_size);
        unsafe {
            let p = self.data_ptr.add(index as usize);
            *p == 0x00 && *p.add(1) == 0x00 && *p.add(2) == 0x01
        }
    }

    /// Write a start-code at the given offset. Returns 3 (the number of bytes written).
    pub fn set_slice_start_code_at_offset(&mut self, index: vk::DeviceSize) -> vk::DeviceSize {
        debug_assert!(!self.data_ptr.is_null());
        debug_assert!(index + 2 < self.max_size);
        unsafe {
            let p = self.data_ptr.add(index as usize);
            *p = 0x00;
            *p.add(1) = 0x00;
            *p.add(2) = 0x01;
        }
        3
    }

    pub fn get_bitstream_ptr(&self) -> *mut u8 {
        debug_assert!(!self.data_ptr.is_null());
        self.data_ptr
    }

    pub fn get_max_size(&self) -> vk::DeviceSize {
        self.max_size
    }

    pub fn get_stream_markers_count(&self) -> u32 {
        if let Some(ref buf) = self.bitstream_buffer {
            debug_assert_eq!(buf.get_stream_markers_count(), self.num_slices);
            buf.get_stream_markers_count()
        } else {
            0
        }
    }

    pub fn add_stream_marker(&mut self, stream_offset: u32) -> u32 {
        self.num_slices += 1;
        // We need mutable access to the inner buffer for markers.
        // Use Arc::get_mut if possible; otherwise this is a design limitation
        // that the caller must ensure unique ownership.
        if let Some(ref mut buf) = self.bitstream_buffer {
            Arc::get_mut(buf)
                .expect("add_stream_marker requires unique ownership of the bitstream buffer")
                .add_stream_marker(stream_offset)
        } else {
            u32::MAX
        }
    }

    pub fn reset_stream_markers(&mut self) -> u32 {
        self.num_slices = 0;
        if let Some(ref mut buf) = self.bitstream_buffer {
            Arc::get_mut(buf)
                .expect("reset_stream_markers requires unique ownership of the bitstream buffer")
                .reset_stream_markers()
        } else {
            0
        }
    }
}

impl Default for VulkanBitstreamBufferStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VulkanBitstreamBufferStream {
    fn drop(&mut self) {
        self.commit_buffer(0);
        self.bitstream_buffer = None;
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Align `value` up to the next multiple of `alignment`.
/// `alignment` must be a power of two.
#[inline]
fn align_up(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
    if alignment == 0 {
        return value;
    }
    (value + (alignment - 1)) & !(alignment - 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up_basic() {
        assert_eq!(align_up(0, 256), 0);
        assert_eq!(align_up(1, 256), 256);
        assert_eq!(align_up(255, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(257, 256), 512);
    }

    #[test]
    fn test_align_up_power_of_two() {
        assert_eq!(align_up(100, 64), 128);
        assert_eq!(align_up(64, 64), 64);
        assert_eq!(align_up(0, 64), 0);
        assert_eq!(align_up(1, 1), 1);
    }

    #[test]
    fn test_align_up_zero_alignment() {
        // With alignment 0, returns value unchanged (guard clause).
        assert_eq!(align_up(42, 0), 42);
    }

    #[test]
    fn test_stream_markers_standalone() {
        // Test stream marker logic without Vulkan by directly manipulating a Vec.
        // This mirrors the marker logic inside VulkanBitstreamBufferImpl.
        let mut markers: Vec<u32> = Vec::with_capacity(256);

        // add
        markers.push(0);
        assert_eq!(markers.len(), 1);
        markers.push(100);
        assert_eq!(markers.len(), 2);

        // get
        assert_eq!(markers[0], 0);
        assert_eq!(markers[1], 100);

        // set
        markers[0] = 50;
        assert_eq!(markers[0], 50);

        // get_markers_ptr equivalent
        let slice = &markers[1..];
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0], 100);

        // reset
        let old_len = markers.len() as u32;
        markers.clear();
        assert_eq!(old_len, 2);
        assert_eq!(markers.len(), 0);
    }

    #[test]
    fn test_buffer_stream_default() {
        let stream = VulkanBitstreamBufferStream::new();
        assert!(!stream.is_valid());
        assert_eq!(stream.get_max_size(), 0);
    }
}
