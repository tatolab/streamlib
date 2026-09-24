// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side generic Vulkan `VkBuffer` — imports a host-allocated
//! DMA-BUF or OPAQUE_FD on Linux, or an IOSurface's pages on macOS, and
//! exposes a CPU-mapped pointer for staging upload / readback.
//! Role-specific shape (pixel `width`/`height`, vertex stride, etc.) lives
//! on the wrapping struct in the calling adapter, not on this primitive.
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
    /// The IOSurface whose pages plane 0's memory is, kept alive past the
    /// memory importing it.
    #[cfg(target_os = "macos")]
    backing_iosurface: Option<objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>>,
}

/// Every import below takes a file descriptor, so the whole block is
/// Linux-only: MoltenVK advertises neither `VK_KHR_external_memory_fd` nor
/// `VK_EXT_external_memory_dma_buf`, and the Apple arm imports an IOSurface
/// rather than a descriptor.
#[cfg(target_os = "linux")]
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
    /// **Never close `fd` on error.** Ownership transfers to the driver
    /// at the successful `vkAllocateMemory` inside this call, not at the
    /// call's own success: every failure up to and including the import
    /// leaves `fd` with the caller, but the bind and the mapping run
    /// after it and free the imported memory — which closes the fd —
    /// before returning. The two groups share error variants, so a caller
    /// cannot tell them apart; closing on error is therefore a
    /// double-close on the arms that already handed it over, and the only
    /// safe rule is to leave it alone.
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
    /// Same fd-ownership rule as [`Self::from_opaque_fd`]: never close
    /// `fd` on error.
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
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
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
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
        })
    }
}

#[cfg(target_os = "macos")]
impl ConsumerVulkanBuffer {
    /// Import `iosurface`'s pages as a single-plane HOST_VISIBLE `VkBuffer`
    /// through `VK_EXT_external_memory_host`, zero-copy: its base address
    /// for its allocation size rounded up to the device's import alignment.
    /// The mapping is the IOSurface's own base address, and the buffer
    /// retains the surface until its memory is freed.
    ///
    /// Never import the surface through a `VkImage`'s memory to map it:
    /// MoltenVK maps a private copy there. No cache mode is set on the
    /// surface — the default mapping is cached; write-combined reads run
    /// ~200x slower and inhibit-cache faults.
    ///
    /// Refused, naming the reason, when the device cannot import host
    /// memory, the base address is off the import alignment, the rounding
    /// would reach past the pages the surface is mapped on, or the driver
    /// declines.
    pub fn from_iosurface_pages(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<Self> {
        let surface_description = format!("{}x{} IOSurface", iosurface.width(), iosurface.height());
        let refusal = |reason: String| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanBuffer::from_iosurface_pages: the {surface_description} {reason}"
            ))
        };
        let import_alignment = vulkan_device
            .imported_host_pointer_alignment()
            .filter(|alignment| *alignment > 0)
            .ok_or_else(|| {
                refusal("cannot be imported: VK_EXT_external_memory_host is not enabled".into())
            })?;
        let base_address = iosurface.base_address().as_ptr().cast::<u8>();
        let allocation_byte_size = iosurface.alloc_size() as u64;
        if allocation_byte_size == 0 {
            return Err(refusal("has no allocation to import".into()));
        }
        if !(base_address as u64).is_multiple_of(import_alignment) {
            return Err(refusal(format!(
                "has base address {base_address:p}, off the driver's {import_alignment}-byte \
                 host-pointer import alignment"
            )));
        }
        // A surface is mapped a whole page at a time and reports an
        // allocation that can end mid-page, so rounding up stays inside the
        // surface for any alignment up to the page size.
        // SAFETY: `sysconf` reads a system constant and touches no memory.
        let page_byte_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .ok()
            .filter(|page_byte_size| *page_byte_size > 0)
            .ok_or_else(|| refusal("cannot be sized: the system page size is unreadable".into()))?;
        let mapped_byte_size = allocation_byte_size.next_multiple_of(page_byte_size);
        let imported_byte_size = allocation_byte_size.next_multiple_of(import_alignment);
        if imported_byte_size > mapped_byte_size {
            return Err(refusal(format!(
                "has a {allocation_byte_size}-byte allocation that rounds up to \
                 {imported_byte_size} bytes on the import alignment, past the \
                 {mapped_byte_size} bytes of pages it is mapped on"
            )));
        }

        let plane = create_bind_and_map_imported_plane(
            vulkan_device,
            imported_byte_size,
            vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT,
            |requirements, _| {
                // Host memory imports exactly the range handed over; a buffer
                // needing more would bind past the surface's pages.
                if requirements.size > imported_byte_size {
                    return Err(refusal(format!(
                        "cannot back a buffer needing {} bytes with its {imported_byte_size}",
                        requirements.size
                    )));
                }
                vulkan_device.import_host_pointer_memory(
                    base_address,
                    imported_byte_size,
                    requirements.memory_type_bits,
                )
            },
        )?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer: plane.buffer,
            imported_memory: plane.memory,
            mapped_ptr: plane.mapped_ptr,
            extra_imported_planes: Vec::new(),
            size: plane.size,
            backing_iosurface: Some(objc2_core_foundation::CFRetained::from(iosurface)),
        })
    }
}

#[cfg(target_os = "macos")]
impl ConsumerVulkanBuffer {
    /// The IOSurface this buffer's memory is, when it was imported from one.
    pub fn backing_iosurface(&self) -> Option<&objc2_io_surface::IOSurfaceRef> {
        self.backing_iosurface.as_deref()
    }

    /// The `MTLBuffer` MoltenVK backs plane 0's memory with, retained for the
    /// caller — for an IOSurface import, a no-copy buffer over the surface's
    /// own pages.
    ///
    /// The buffer addresses memory this import pins, so it must not outlive
    /// `self`. Refused when the device lacks `VK_EXT_metal_objects` or
    /// MoltenVK hands back no buffer.
    pub fn exported_metal_buffer(
        &self,
    ) -> Result<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        use vulkanalia::vk::ExtMetalObjectsExtensionDeviceCommands;

        if !self.vulkan_device.supports_metal_objects_interop() {
            return Err(ConsumerRhiError::Gpu(
                "ConsumerVulkanBuffer::exported_metal_buffer: VK_EXT_metal_objects is not \
                 enabled on this device, so no MTLBuffer can be exported"
                    .into(),
            ));
        }
        let mut buffer_info = vk::ExportMetalBufferInfoEXT::builder()
            .memory(self.imported_memory)
            .build();
        let mut objects_info = vk::ExportMetalObjectsInfoEXT::builder().build();
        objects_info.next = (&mut buffer_info as *mut _) as *const std::ffi::c_void;
        // SAFETY: the extension is enabled (checked above) and the chain names
        // this import's own memory; MoltenVK writes the buffer pointer back.
        unsafe {
            self.vulkan_device
                .device()
                .export_metal_objects_ext(&mut objects_info)
        };
        let metal_buffer_pointer = buffer_info.mtl_buffer
            as *mut objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>;
        // SAFETY: MoltenVK returns the memory's own `MTLBuffer`, alive for the
        // memory's lifetime and not retained for the caller; retaining here
        // gives the caller its own reference.
        unsafe { objc2::rc::Retained::retain(metal_buffer_pointer) }.ok_or_else(|| {
            ConsumerRhiError::Gpu(
                "ConsumerVulkanBuffer::exported_metal_buffer: vkExportMetalObjectsEXT returned \
                 no MTLBuffer for the imported memory"
                    .into(),
            )
        })
    }
}

impl ConsumerVulkanBuffer {
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
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn import_single_plane_with_handle_type(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    fd: std::os::unix::io::RawFd,
    effective_size: vk::DeviceSize,
    handle_type: ImportHandleType,
) -> Result<ConsumerImportedPlane> {
    let vk_handle_type = match handle_type {
        ImportHandleType::DmaBuf => vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        ImportHandleType::OpaqueFdAtFirstMatchingMemoryType
        | ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(_) => {
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD
        }
    };
    let host_visible =
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    create_bind_and_map_imported_plane(
        vulkan_device,
        effective_size,
        vk_handle_type,
        |requirements, alloc_size| match handle_type {
            ImportHandleType::DmaBuf => vulkan_device.import_dma_buf_memory(
                fd,
                alloc_size,
                requirements.memory_type_bits,
                host_visible,
            ),
            ImportHandleType::OpaqueFdAtFirstMatchingMemoryType => vulkan_device
                .import_opaque_fd_memory(
                    fd,
                    alloc_size,
                    requirements.memory_type_bits,
                    host_visible,
                ),
            ImportHandleType::OpaqueFdAtStatedMemoryTypeIndex(stated_memory_type_index) => {
                refuse_unless_the_buffer_can_bind_the_stated_memory_type_index(
                    requirements.memory_type_bits,
                    stated_memory_type_index,
                )?;
                vulkan_device.import_opaque_fd_memory_at_stated_memory_type_index(
                    fd,
                    alloc_size,
                    stated_memory_type_index,
                )
            }
        },
    )
}

/// Create a `VkBuffer` of `effective_size` for external memory of
/// `vk_handle_type`, bind the memory `import_memory` imports for it, and map
/// it. `import_memory` is handed the buffer's requirements and the
/// allocation size — `effective_size` or the requirements' size, whichever
/// is larger. Every failure unwinds what was created before it.
fn create_bind_and_map_imported_plane(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    effective_size: vk::DeviceSize,
    vk_handle_type: vk::ExternalMemoryHandleTypeFlags,
    import_memory: impl FnOnce(&vk::MemoryRequirements, vk::DeviceSize) -> Result<vk::DeviceMemory>,
) -> Result<ConsumerImportedPlane> {
    let device = vulkan_device.device();
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

    // SAFETY: `buffer_info` and the struct it chains outlive the call.
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|e| {
        ConsumerRhiError::Gpu(format!("ConsumerVulkanBuffer: create_buffer failed: {e}"))
    })?;
    // SAFETY: `buffer` was created on this device just above; every
    // `destroy_buffer` below runs once, on a failure path that returns.
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let alloc_size = effective_size.max(requirements.size);

    let memory = import_memory(&requirements, alloc_size)
        .inspect_err(|_| unsafe { device.destroy_buffer(buffer, None) })?;

    // SAFETY: `memory` was imported for this buffer's requirements and is
    // bound once, at offset 0.
    if let Err(e) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        vulkan_device.free_imported_memory(memory);
        unsafe { device.destroy_buffer(buffer, None) };
        return Err(ConsumerRhiError::Gpu(format!(
            "ConsumerVulkanBuffer: bind_buffer_memory failed: {e}"
        )));
    }

    let mapped_ptr = vulkan_device
        .map_imported_memory(memory, effective_size)
        .inspect_err(|_| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
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
#[cfg(target_os = "linux")]
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

// SAFETY: the handles are used only through the device's own externally
// synchronised calls, and the mapping is plain memory. The backing
// IOSurface's retain, lock and use-count calls are thread-safe; `CFRetained`
// is not marked `Send`/`Sync` only because not every CoreFoundation type is.
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

// The check it locks guards an OPAQUE_FD bind, which is Linux-only along with
// every other descriptor import on this type.
#[cfg(all(test, target_os = "linux"))]
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

#[cfg(all(test, target_os = "macos"))]
mod iosurface_import_tests {
    use super::*;
    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_io_surface::{
        IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight,
        kIOSurfaceWidth,
    };

    fn a_private_bgra_iosurface(width: u32, height: u32) -> CFRetained<IOSurfaceRef> {
        let width_number = CFNumber::new_i64(i64::from(width));
        let height_number = CFNumber::new_i64(i64::from(height));
        let bytes_per_element_number = CFNumber::new_i64(4);
        // SAFETY: the IOSurface property keys are immutable framework statics.
        let keys: [&CFString; 3] =
            unsafe { [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement] };
        let values: [&CFType; 3] = [&width_number, &height_number, &bytes_per_element_number];
        let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: the dictionary holds only documented keys with CFNumber values.
        unsafe { IOSurfaceRef::new(properties.as_opaque()) }.expect("IOSurfaceCreate")
    }

    fn try_create_device() -> Option<Arc<ConsumerVulkanDevice>> {
        match ConsumerVulkanDevice::new() {
            Ok(device) => Some(Arc::new(device)),
            Err(unavailable) => {
                println!("Skipping test — ConsumerVulkanDevice unavailable: {unavailable}");
                None
            }
        }
    }

    /// The mapping is the surface's own memory, not a driver copy: a write
    /// through it reads back through the surface's own base address.
    #[test]
    fn an_iosurface_import_maps_the_surfaces_own_pages() {
        let Some(device) = try_create_device() else {
            return;
        };
        let iosurface = a_private_bgra_iosurface(64, 32);
        let buffer = ConsumerVulkanBuffer::from_iosurface_pages(&device, &iosurface)
            .expect("an IOSurface imports as host memory");

        assert_eq!(
            buffer.mapped_ptr(),
            iosurface.base_address().as_ptr().cast::<u8>(),
            "the mapping must be the IOSurface's base address, never a private copy"
        );
        assert!(buffer.size() >= iosurface.alloc_size() as u64);

        unsafe { iosurface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        // SAFETY: the mapping spans the surface's allocation.
        unsafe { buffer.mapped_ptr().add(17).write(0xA5) };
        let read_through_the_surface = unsafe {
            iosurface
                .base_address()
                .as_ptr()
                .cast::<u8>()
                .add(17)
                .read()
        };
        unsafe { iosurface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        assert_eq!(read_through_the_surface, 0xA5);
    }

    /// The exported `MTLBuffer` is the surface's own pages with no copy: its
    /// contents pointer is the surface's base address, and a store through
    /// it reads back through the surface.
    #[test]
    fn an_iosurface_import_exports_a_metal_buffer_over_the_surfaces_own_pages() {
        use objc2_metal::MTLBuffer as _;

        let Some(device) = try_create_device() else {
            return;
        };
        if !device.supports_metal_objects_interop() {
            println!("Skipping test — the device has no VK_EXT_metal_objects");
            return;
        }
        let iosurface = a_private_bgra_iosurface(1000, 8);
        let buffer = ConsumerVulkanBuffer::from_iosurface_pages(&device, &iosurface)
            .expect("an IOSurface imports as host memory");

        let metal_buffer = buffer
            .exported_metal_buffer()
            .expect("the import's memory exports its MTLBuffer");

        assert_eq!(
            metal_buffer.contents().as_ptr().cast::<u8>(),
            iosurface.base_address().as_ptr().cast::<u8>(),
            "the MTLBuffer must alias the surface's pages, never a private copy"
        );
        assert!(metal_buffer.length() as u64 >= iosurface.alloc_size() as u64);
        // SAFETY: the buffer spans the surface's allocation.
        unsafe {
            metal_buffer
                .contents()
                .as_ptr()
                .cast::<u8>()
                .add(29)
                .write(0x5A)
        };
        let read_through_the_surface = unsafe {
            iosurface
                .base_address()
                .as_ptr()
                .cast::<u8>()
                .add(29)
                .read()
        };
        assert_eq!(read_through_the_surface, 0x5A);
    }

    /// Importing takes a retain on the surface, never a use count, so a
    /// cached import does not report the surface in use.
    #[test]
    fn an_iosurface_import_does_not_mark_the_surface_in_use() {
        let Some(device) = try_create_device() else {
            return;
        };
        let iosurface = a_private_bgra_iosurface(16, 16);
        let buffer = ConsumerVulkanBuffer::from_iosurface_pages(&device, &iosurface)
            .expect("an IOSurface imports as host memory");
        assert!(!iosurface.is_in_use());
        drop(buffer);
        assert_eq!(device.live_import_allocation_count(), 0);
    }
}
