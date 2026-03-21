// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::Mutex;

use ash::vk;
use ash::vk::Handle;

use crate::core::{Result, StreamError};

/// Vulkan texture cache — creates and caches VkImageView from VkImage.
///
/// Vulkan equivalent of CVMetalTextureCache on macOS.
pub struct VulkanTextureCache {
    device: ash::Device,
    view_cache: Mutex<HashMap<u64, vk::ImageView>>,
}

impl VulkanTextureCache {
    /// Create a new texture cache for the given Vulkan device.
    pub fn new(device: &ash::Device) -> Self {
        Self {
            device: device.clone(),
            view_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Create or retrieve a cached VkImageView for the given VkImage.
    pub fn create_view_from_image(
        &self,
        image: vk::Image,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> Result<vk::ImageView> {
        let key = image.as_raw();

        let mut cache = self
            .view_cache
            .lock()
            .map_err(|e| StreamError::GpuError(format!("Failed to lock texture cache: {e}")))?;

        if let Some(&existing_view) = cache.get(&key) {
            return Ok(existing_view);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });

        let _ = width;
        let _ = height;

        let view = unsafe { self.device.create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create VkImageView: {e}")))?;

        cache.insert(key, view);
        Ok(view)
    }

    /// Destroy all cached views and clear the cache.
    pub fn flush(&self) {
        if let Ok(mut cache) = self.view_cache.lock() {
            for (_, view) in cache.drain() {
                unsafe {
                    self.device.destroy_image_view(view, None);
                }
            }
        }
    }
}

impl Drop for VulkanTextureCache {
    fn drop(&mut self) {
        self.flush();
    }
}

// VulkanTextureCache is thread-safe: Vulkan handles are externally synchronized via Mutex
unsafe impl Send for VulkanTextureCache {}
unsafe impl Sync for VulkanTextureCache {}
