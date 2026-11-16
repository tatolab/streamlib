// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Importing DMA-BUF, OPAQUE_FD and host-pointer memory, and freeing, mapping and counting what was imported.

use super::*;

impl ConsumerVulkanDevice {
    /// Find the first memory type whose bit is set in `type_filter` and
    /// satisfies `required_properties`.
    pub(crate) fn find_memory_type(
        &self,
        type_filter: u32,
        required_properties: vk::MemoryPropertyFlags,
    ) -> Result<u32> {
        if required_properties.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            // Prefer pure DEVICE_LOCAL (main VRAM heap) over BAR aperture
            // when DEVICE_LOCAL is requested.
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                {
                    return Ok(i);
                }
            }
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                {
                    return Ok(i);
                }
            }
        }
        for i in 0..self.memory_properties.memory_type_count {
            let flags = self.memory_properties.memory_types[i as usize].property_flags;
            if (type_filter & (1 << i)) != 0 && flags.contains(required_properties) {
                return Ok(i);
            }
        }
        Err(ConsumerRhiError::Gpu(format!(
            "ConsumerVulkanDevice: no suitable memory type (filter=0x{:x}, required={:?})",
            type_filter, required_properties
        )))
    }

    /// Import a DMA-BUF file descriptor as `VkDeviceMemory`. Pairs with
    /// the host's `export_dma_buf_fd`: the host allocates, exports,
    /// and registers in surface-share; the consumer looks up, imports,
    /// and binds.
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// `fd`. Refused by name on a device without
    /// `VK_EXT_external_memory_dma_buf`.
    pub fn import_dma_buf_memory(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
        preferred_flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        self.refuse_unless_device_extension_is_enabled(
            "ConsumerVulkanDevice::import_dma_buf_memory",
            vk::EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION,
            "no DMA-BUF can be imported",
        )?;
        let memory_type_index = self.find_memory_type(memory_type_bits, preferred_flags)?;

        let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd)
            .build();

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: import_dma_buf_memory failed: {e}"
            ))
        })?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "ConsumerVulkanDevice: imported DMA-BUF ({} bytes, type={}, live={})",
            allocation_size,
            memory_type_index,
            count
        );

        Ok(memory)
    }

    /// Import an OPAQUE_FD file descriptor as `VkDeviceMemory` bound to
    /// the memory type index the **exporter** allocated from. Pairs with
    /// the host's `export_opaque_fd_memory`; for DMA-BUF FDs use
    /// [`Self::import_dma_buf_memory`].
    ///
    /// The conforming import for this handle type: OPAQUE_FD has no
    /// `vkGetMemoryFdPropertiesKHR` query, so an importer cannot derive a
    /// compatible index and must be told the exporter's — which is why
    /// the surface-share registration publishes it.
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// `fd`. Pairs with [`Self::free_imported_memory`].
    #[tracing::instrument(level = "trace", skip(self), fields(fd, allocation_size))]
    pub fn import_opaque_fd_memory_at_stated_memory_type_index(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_index: u32,
    ) -> Result<vk::DeviceMemory> {
        self.import_opaque_fd_memory(fd, allocation_size, memory_type_index, None)
    }

    /// [`Self::import_opaque_fd_memory_at_stated_memory_type_index`] for an
    /// exporter's dedicated image allocation, imported dedicated to `image`,
    /// the image it will bind. Same fd ownership rules.
    ///
    /// An image import that is not dedicated binds as a plain allocation,
    /// and on NVIDIA some such imports sample as all zeros.
    #[tracing::instrument(level = "trace", skip(self), fields(fd, allocation_size))]
    pub fn import_opaque_fd_memory_dedicated_to_image_at_stated_memory_type_index(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_index: u32,
        image: vk::Image,
    ) -> Result<vk::DeviceMemory> {
        self.import_opaque_fd_memory(fd, allocation_size, memory_type_index, Some(image))
    }

    fn import_opaque_fd_memory(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_index: u32,
        dedicated_to_image: Option<vk::Image>,
    ) -> Result<vk::DeviceMemory> {
        let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();
        let mut dedicated_allocation_info = vk::MemoryDedicatedAllocateInfo::builder()
            .image(dedicated_to_image.unwrap_or_default())
            .build();

        let mut alloc_info_builder = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info);
        if dedicated_to_image.is_some() {
            alloc_info_builder = alloc_info_builder.push_next(&mut dedicated_allocation_info);
        }
        let alloc_info = alloc_info_builder.build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: OPAQUE_FD memory import failed: {e}"
            ))
        })?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "ConsumerVulkanDevice: imported OPAQUE_FD ({} bytes, type={}, live={})",
            allocation_size,
            memory_type_index,
            count
        );

        Ok(memory)
    }

    /// Import a caller-owned host range as `VkDeviceMemory` through
    /// `VK_EXT_external_memory_host`, on a HOST_CACHED type where one takes
    /// it. The driver pins the range, never copies it, so it must outlive
    /// the memory. Pairs with [`Self::free_imported_memory`].
    pub fn import_host_pointer_memory(
        &self,
        host_ptr: *mut u8,
        byte_len: vk::DeviceSize,
        memory_type_bits: u32,
    ) -> Result<vk::DeviceMemory> {
        use vulkanalia::vk::ExtExternalMemoryHostExtensionDeviceCommands as _;

        // The memory type must suit the host range as well as the buffer
        // (VUID-VkMemoryAllocateInfo-memoryTypeIndex-01744).
        let mut host_pointer_properties = vk::MemoryHostPointerPropertiesEXT::default();
        // SAFETY: `host_ptr` is the caller's live range; the properties struct
        // outlives the call.
        unsafe {
            self.device.get_memory_host_pointer_properties_ext(
                vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT,
                host_ptr.cast_const().cast(),
                &mut host_pointer_properties,
            )
        }
        .map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: the driver cannot describe the host range {host_ptr:p}: {e}"
            ))
        })?;
        let memory_type_bits = memory_type_bits & host_pointer_properties.memory_type_bits;
        if memory_type_bits == 0 {
            return Err(ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: no memory type suits both the buffer and the host range \
                 {host_ptr:p}"
            )));
        }
        let host_visible =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory_type_index = self
            .find_memory_type(
                memory_type_bits,
                host_visible | vk::MemoryPropertyFlags::HOST_CACHED,
            )
            .or_else(|_| self.find_memory_type(memory_type_bits, host_visible))?;

        let mut import_info = vk::ImportMemoryHostPointerInfoEXT::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT)
            .build();
        import_info.host_pointer = host_ptr.cast();
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(byte_len)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: the driver refused to import the host range \
                 {host_ptr:p}+{byte_len}: {e}"
            ))
        })?;
        self.live_allocation_count.fetch_add(1, Ordering::Relaxed);
        Ok(memory)
    }

    /// Free imported memory. Pair with [`Self::import_dma_buf_memory`],
    /// [`Self::import_opaque_fd_memory_at_stated_memory_type_index`] or
    /// [`Self::import_host_pointer_memory`]. Calling on memory not allocated
    /// through one of those methods is undefined behavior at the Vulkan
    /// level.
    pub fn free_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.free_memory(memory, None) };
        self.live_allocation_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Map imported device memory for CPU access. The returned pointer
    /// is valid until [`Self::unmap_imported_memory`] is called.
    pub fn map_imported_memory(
        &self,
        memory: vk::DeviceMemory,
        size: vk::DeviceSize,
    ) -> Result<*mut u8> {
        let ptr = unsafe {
            self.device
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: map_imported_memory failed: {e}"
            ))
        })?;
        Ok(ptr as *mut u8)
    }

    /// Unmap a previously-mapped imported memory region.
    pub fn unmap_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.unmap_memory(memory) };
    }

    /// Current number of live DMA-BUF imports through this device.
    /// Surfaced for tracing + leak detection on Drop.
    pub fn live_import_allocation_count(&self) -> usize {
        self.live_allocation_count.load(Ordering::Relaxed)
    }
}
