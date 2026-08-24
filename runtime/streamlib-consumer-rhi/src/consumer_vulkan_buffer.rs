// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side generic Vulkan `VkBuffer` — imports a host-allocated
//! DMA-BUF or OPAQUE_FD and exposes a CPU-mapped pointer for staging
//! upload / readback. Role-specific shape (pixel `width`/`height`,
//! vertex stride, etc.) lives on the wrapping struct in the calling
//! adapter, not on this primitive.
//!
//! Mirrors [`crate::ConsumerVulkanTexture`] for buffer handles.
//! Single-plane and multi-plane import constructors only — no
//! allocation, no DMA-BUF export.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::{ConsumerRhiError, ConsumerVulkanDevice, Result, VulkanRhiBuffer};

/// One imported plane: buffer + memory + mapped pointer + size.
struct ConsumerImportedPlane {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
}

/// Consumer-side imported `VkBuffer`. See module docs.
pub struct ConsumerVulkanBuffer {
    vulkan_device: Arc<ConsumerVulkanDevice>,
    /// Plane 0's `VkBuffer`. Single-plane imports use only this; multi-
    /// plane imports keep planes 1..N in [`Self::extra_imported_planes`].
    buffer: vk::Buffer,
    imported_memory: vk::DeviceMemory,
    /// Persistently mapped CPU pointer for plane 0.
    mapped_ptr: *mut u8,
    extra_imported_planes: Vec<ConsumerImportedPlane>,
    /// Size of plane 0 in bytes.
    size: vk::DeviceSize,
}

impl ConsumerVulkanBuffer {
    /// Import a single-plane DMA-BUF as a HOST_VISIBLE `VkBuffer`.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, allocation_size))]
    pub fn from_dma_buf_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        Self::from_dma_buf_fds(vulkan_device, &[fd], &[allocation_size])
    }

    /// Import an OPAQUE_FD as a HOST_VISIBLE `VkBuffer`.
    ///
    /// Pairs with the host's
    /// [`crate::HostVulkanBuffer::new_opaque_fd_export`] +
    /// `export_opaque_fd_memory`. This is the constructor CUDA cdylibs
    /// use after looking up a surface registered with
    /// `handle_type="opaque_fd"` on the surface-share wire — the resulting
    /// `VkBuffer`'s memory is also what `cudaImportExternalMemory` reaches
    /// for via the same FD.
    ///
    /// Single-FD only: OPAQUE_FD has no multi-plane semantics (CUDA imports
    /// flat memory; multi-plane DMA-BUFs go through [`Self::from_dma_buf_fds`]).
    ///
    /// fd ownership transfers to the Vulkan driver at the successful
    /// `vkAllocateMemory` inside this call, not at the call's own
    /// success: the bind and the mapping run after the import, and this
    /// frees the imported memory — which closes the fd — before
    /// returning their failures. Only the refusals raised *before* the
    /// import leave `fd` with the caller (a zero `allocation_size`, a
    /// stated index this buffer cannot bind, a failed `create_buffer`).
    /// A caller that closes `fd` on every error double-closes.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, allocation_size))]
    pub fn from_opaque_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        allocation_size: vk::DeviceSize,
    ) -> Result<Self> {
        Self::from_opaque_fd_with_handle_type(
            vulkan_device,
            fd,
            allocation_size,
            ImportHandleType::OpaqueFdAtFirstMatchingMemoryType,
        )
    }

    /// Import an OPAQUE_FD as a HOST_VISIBLE `VkBuffer`, binding the
    /// memory type index the **exporter** allocated from.
    ///
    /// The conforming import for this handle type — see
    /// [`ConsumerVulkanDevice::import_opaque_fd_memory_at_stated_memory_type_index`].
    /// `stated_memory_type_index` is the surface-share registration's
    /// `vk_memory_type_index`; a value the imported buffer cannot bind is
    /// refused here by name rather than tripping
    /// VUID-vkBindBufferMemory-memory-01035 inside the driver.
    ///
    /// Same fd-ownership rule as [`Self::from_opaque_fd`]: the driver
    /// takes the fd at the import, so only the refusals raised before it
    /// — this one included — leave `fd` with the caller.
    #[tracing::instrument(level = "trace", skip(vulkan_device), fields(fd, allocation_size))]
    pub fn from_opaque_fd_at_stated_memory_type_index(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        allocation_size: vk::DeviceSize,
        stated_memory_type_index: u32,
    ) -> Result<Self> {
        Self::from_opaque_fd_with_handle_type(
            vulkan_device,
            fd,
            allocation_size,
            ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(stated_memory_type_index),
        )
    }

    fn from_opaque_fd_with_handle_type(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
        allocation_size: vk::DeviceSize,
        handle_type: ImportHandleType,
    ) -> Result<Self> {
        if allocation_size == 0 {
            return Err(ConsumerRhiError::Configuration(
                "ConsumerVulkanBuffer: an OPAQUE_FD import needs allocation_size > 0".into(),
            ));
        }

        let plane =
            import_single_plane_with_handle_type(vulkan_device, fd, allocation_size, handle_type)?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            imported_memory: plane.memory,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            size: plane.size,
        })
    }

    /// Import N planes from N DMA-BUF FDs — each gets its own
    /// `VkBuffer` + imported `VkDeviceMemory` + mapping. `plane_sizes[i]`
    /// must be the non-zero allocation size of plane `i`.
    ///
    /// Partial-failure semantics: every plane that succeeded is torn
    /// down before the error is returned. fd ownership transfers to
    /// the Vulkan driver on success per plane.
    #[tracing::instrument(level = "trace", skip(vulkan_device, fds, plane_sizes), fields(plane_count = fds.len()))]
    pub fn from_dma_buf_fds(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fds: &[std::os::unix::io::RawFd],
        plane_sizes: &[vk::DeviceSize],
    ) -> Result<Self> {
        if fds.is_empty() {
            return Err(ConsumerRhiError::Configuration(
                "ConsumerVulkanBuffer: fd vec must be non-empty".into(),
            ));
        }
        if fds.len() != plane_sizes.len() {
            return Err(ConsumerRhiError::Configuration(format!(
                "ConsumerVulkanBuffer: plane_sizes length ({}) must match fds length ({})",
                plane_sizes.len(),
                fds.len()
            )));
        }
        if fds.len() > streamlib_surface_client::MAX_DMA_BUF_PLANES {
            return Err(ConsumerRhiError::Configuration(format!(
                "ConsumerVulkanBuffer: plane count {} exceeds MAX_DMA_BUF_PLANES ({})",
                fds.len(),
                streamlib_surface_client::MAX_DMA_BUF_PLANES
            )));
        }

        let mut imported: Vec<ConsumerImportedPlane> = Vec::with_capacity(fds.len());
        for (idx, (&fd, &plane_size)) in fds.iter().zip(plane_sizes.iter()).enumerate() {
            if plane_size == 0 {
                for plane in imported.into_iter() {
                    teardown_plane(vulkan_device, plane);
                }
                return Err(ConsumerRhiError::Configuration(format!(
                    "ConsumerVulkanBuffer: plane {idx} has size=0 — caller must supply \
                     each plane's allocation size"
                )));
            }

            match import_single_plane(vulkan_device, fd, plane_size) {
                Ok(plane) => imported.push(plane),
                Err(e) => {
                    for plane in imported.into_iter() {
                        teardown_plane(vulkan_device, plane);
                    }
                    return Err(e);
                }
            }
        }

        let plane0 = imported.remove(0);
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane0.buffer,
            imported_memory: plane0.memory,
            mapped_ptr: plane0.mapped_ptr,
            extra_imported_planes: imported,
            size: plane0.size,
        })
    }

    /// Persistently mapped CPU pointer for plane 0. Use
    /// [`Self::plane_mapped_ptr`] for any plane.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Number of planes — `1` for single-plane imports, `N` for multi-
    /// plane.
    pub fn plane_count(&self) -> u32 {
        1 + self.extra_imported_planes.len() as u32
    }

    /// Mapped CPU pointer for plane `plane_index`, or null if out of
    /// range.
    pub fn plane_mapped_ptr(&self, plane_index: u32) -> *mut u8 {
        if plane_index == 0 {
            return self.mapped_ptr;
        }
        self.extra_imported_planes
            .get(plane_index as usize - 1)
            .map(|p| p.mapped_ptr)
            .unwrap_or(std::ptr::null_mut())
    }

    /// Byte size of plane `plane_index`, or `0` if out of range.
    pub fn plane_size(&self, plane_index: u32) -> vk::DeviceSize {
        if plane_index == 0 {
            return self.size;
        }
        self.extra_imported_planes
            .get(plane_index as usize - 1)
            .map(|p| p.size)
            .unwrap_or(0)
    }

    /// Plane 0 size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Underlying `VkBuffer` for plane 0.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }
}

/// Which `vkImportMemoryFdInfoKHR.handleType` to chain through when
/// importing a plane, and how the memory type index is arrived at.
#[derive(Copy, Clone, Debug)]
enum ImportHandleType {
    DmaBuf,
    /// The importer searches for a memory type itself. Correct for
    /// DMA-BUF-shaped negotiation, and a guess for OPAQUE_FD — it agrees
    /// with the exporter only where both land on the same type.
    OpaqueFdAtFirstMatchingMemoryType,
    /// The exporter's own memory type index, as published on the
    /// surface-share wire.
    OpaqueFdAtStatedMemoryTypeIndex(u32),
}

fn import_single_plane(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
) -> Result<ConsumerImportedPlane> {
    import_single_plane_with_handle_type(
        vulkan_device,
        fd,
        effective_size,
        ImportHandleType::DmaBuf,
    )
}

fn import_single_plane_with_handle_type(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
    handle_type: ImportHandleType,
) -> Result<ConsumerImportedPlane> {
    let device = vulkan_device.device();

    let vk_handle_type = match handle_type {
        ImportHandleType::DmaBuf => vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        ImportHandleType::OpaqueFdAtFirstMatchingMemoryType
        | ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(_) => {
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD
        }
    };

    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk_handle_type)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(effective_size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info)
        .build();

    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|e| {
        ConsumerRhiError::Gpu(format!("ConsumerVulkanBuffer: create_buffer failed: {e}"))
    })?;

    let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let alloc_size = effective_size.max(mem_requirements.size);

    if let ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(stated_memory_type_index) = handle_type
        && let Err(refusal) = refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
            mem_requirements.memory_type_bits,
            stated_memory_type_index,
        )
    {
        unsafe { device.destroy_buffer(buffer, None) };
        return Err(refusal);
    }

    let memory = match handle_type {
        ImportHandleType::DmaBuf => vulkan_device.import_dma_buf_memory(
            fd,
            alloc_size,
            mem_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ),
        ImportHandleType::OpaqueFdAtFirstMatchingMemoryType => vulkan_device
            .import_opaque_fd_memory(
                fd,
                alloc_size,
                mem_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ),
        ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(stated_memory_type_index) => {
            vulkan_device.import_opaque_fd_memory_at_stated_memory_type_index(
                fd,
                alloc_size,
                stated_memory_type_index,
            )
        }
    }
    .map_err(|e| {
        unsafe { device.destroy_buffer(buffer, None) };
        e
    })?;

    unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
        vulkan_device.free_imported_memory(memory);
        unsafe { device.destroy_buffer(buffer, None) };
        ConsumerRhiError::Gpu(format!(
            "ConsumerVulkanBuffer: bind_buffer_memory failed: {e}"
        ))
    })?;

    let mapped_ptr = vulkan_device
        .map_imported_memory(memory, effective_size)
        .map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            e
        })?;

    Ok(ConsumerImportedPlane {
        buffer,
        memory,
        mapped_ptr,
        size: effective_size,
    })
}

/// Refuse a stated memory type index the buffer's own
/// `VkMemoryRequirements::memoryTypeBits` cannot take.
///
/// `checked_shl` rather than a bare shift: an index at or past
/// VK_MAX_MEMORY_TYPES names no memory type on any device, and must be
/// refused rather than overflow the bit test.
fn refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
    memory_type_bits: u32,
    stated_memory_type_index: u32,
) -> Result<()> {
    let stated_memory_type_bit = 1u32.checked_shl(stated_memory_type_index).unwrap_or(0);
    if memory_type_bits & stated_memory_type_bit != 0 {
        return Ok(());
    }
    Err(ConsumerRhiError::Configuration(format!(
        "ConsumerVulkanBuffer: the exporter states memory type index \
         {stated_memory_type_index}, which this buffer cannot bind \
         (memoryTypeBits=0x{memory_type_bits:x}) — the exporter and importer disagree \
         on the buffer's shape, and binding anyway is undefined behaviour"
    )))
}

fn teardown_plane(vulkan_device: &Arc<ConsumerVulkanDevice>, plane: ConsumerImportedPlane) {
    unsafe { vulkan_device.device().destroy_buffer(plane.buffer, None) };
    vulkan_device.unmap_imported_memory(plane.memory);
    vulkan_device.free_imported_memory(plane.memory);
}

impl Drop for ConsumerVulkanBuffer {
    fn drop(&mut self) {
        unsafe {
            self.vulkan_device
                .device()
                .destroy_buffer(self.buffer, None);
        }
        self.vulkan_device
            .unmap_imported_memory(self.imported_memory);
        self.vulkan_device
            .free_imported_memory(self.imported_memory);
        for plane in self.extra_imported_planes.drain(..) {
            teardown_plane(&self.vulkan_device, plane);
        }
    }
}

unsafe impl Send for ConsumerVulkanBuffer {}
unsafe impl Sync for ConsumerVulkanBuffer {}

impl VulkanRhiBuffer for ConsumerVulkanBuffer {
    fn buffer(&self) -> vk::Buffer {
        ConsumerVulkanBuffer::buffer(self)
    }
    fn mapped_ptr(&self) -> *mut u8 {
        ConsumerVulkanBuffer::mapped_ptr(self)
    }
    fn size(&self) -> vk::DeviceSize {
        ConsumerVulkanBuffer::size(self)
    }
}

#[cfg(test)]
mod stated_memory_type_index_tests {
    use super::*;

    /// The bind check is a pure bit test over the buffer's own
    /// `memoryTypeBits`, so it locks with no device: OPAQUE_FD carries no
    /// `vkGetMemoryFdPropertiesKHR`, and binding an index the buffer
    /// cannot take is undefined behaviour rather than a Vulkan error.
    #[test]
    fn an_index_the_buffer_can_bind_is_accepted_and_one_it_cannot_is_refused() {
        // types 0, 2 and 31 allowed.
        let memory_type_bits = 0b1000_0000_0000_0000_0000_0000_0000_0101u32;

        for allowed in [0u32, 2, 31] {
            assert!(
                refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
                    memory_type_bits,
                    allowed
                )
                .is_ok(),
                "index {allowed} is set in memoryTypeBits and must bind"
            );
        }
        for refused in [1u32, 3, 30] {
            assert!(
                refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
                    memory_type_bits,
                    refused
                )
                .is_err(),
                "index {refused} is clear in memoryTypeBits and must be refused"
            );
        }
    }

    /// An index at or past VK_MAX_MEMORY_TYPES names no memory type. The
    /// bit test must answer "refused" rather than shift out of range —
    /// `1u32 << 32` panics in debug and wraps to bit 0 in release, which
    /// would let `u32::MAX` bind whatever type 0 happens to be.
    #[test]
    fn an_index_past_vk_max_memory_types_is_refused_rather_than_overflowing() {
        let every_type_allowed = u32::MAX;
        assert!(
            refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(every_type_allowed, 31)
                .is_ok(),
            "31 is the last real memory type index"
        );
        for past_the_end in [32u32, 33, 64, u32::MAX] {
            assert!(
                refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
                    every_type_allowed,
                    past_the_end
                )
                .is_err(),
                "index {past_the_end} names no memory type even when every bit is set"
            );
        }
    }

    /// The refusal has to be actionable: it names the index the exporter
    /// stated and the mask the importer derived, which is the whole
    /// diagnosis of an exporter/importer disagreement.
    #[test]
    fn the_refusal_names_the_stated_index_and_the_buffers_memory_type_bits() {
        let refusal = refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(0b1010, 7)
            .expect_err("index 7 is clear in 0b1010");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("memory type index 7"),
            "must name the stated index: {refusal}"
        );
        assert!(
            refusal.contains("memoryTypeBits=0xa"),
            "must name what the buffer can bind: {refusal}"
        );
    }
}
