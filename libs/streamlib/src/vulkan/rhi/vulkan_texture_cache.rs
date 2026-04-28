// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::Mutex;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Result, StreamError};

/// Vulkan texture cache — creates and caches VkImageView from VkImage.
///
/// Vulkan equivalent of CVMetalTextureCache on macOS.
pub struct VulkanTextureCache {
    device: vulkanalia::Device,
    view_cache: Mutex<HashMap<vk::Image, vk::ImageView>>,
}

impl VulkanTextureCache {
    /// Create a new texture cache for the given Vulkan device.
    pub fn new(device: &vulkanalia::Device) -> Self {
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
        let key = image;

        let mut cache = self
            .view_cache
            .lock()
            .map_err(|e| StreamError::GpuError(format!("Failed to lock texture cache: {e}")))?;

        if let Some(&existing_view) = cache.get(&key) {
            return Ok(existing_view);
        }

        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
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
            })
            .build();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{TextureDescriptor, TextureFormat};
    use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTexture};
    use std::sync::Arc;

    #[test]
    fn test_creates_image_view_for_valid_image() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");
        let image = match texture.image() {
            Some(i) => i,
            None => {
                println!("Skipping - texture has no image handle");
                return;
            }
        };

        let cache = VulkanTextureCache::new(device.device());
        let view = cache
            .create_view_from_image(image, vk::Format::B8G8R8A8_UNORM, 64, 64)
            .expect("image view creation failed");

        assert_ne!(view, vk::ImageView::null(), "image view handle must be non-null");
    }

    #[test]
    fn test_returns_cached_view_for_same_image() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm);
        let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");
        let image = match texture.image() {
            Some(i) => i,
            None => {
                println!("Skipping - texture has no image handle");
                return;
            }
        };

        let cache = VulkanTextureCache::new(device.device());

        let view_a = cache
            .create_view_from_image(image, vk::Format::B8G8R8A8_UNORM, 64, 64)
            .expect("first call failed");
        let view_b = cache
            .create_view_from_image(image, vk::Format::B8G8R8A8_UNORM, 64, 64)
            .expect("second call failed");

        assert_eq!(view_a, view_b, "same image must return the same cached VkImageView");
    }

    #[test]
    fn test_flush_destroys_all_cached_views() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let cache = VulkanTextureCache::new(device.device());

        // Create views for two textures so the cache is non-empty
        for _ in 0..2 {
            let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm);
            let texture = HostVulkanTexture::new(&device, &desc).expect("texture creation failed");
            if let Some(image) = texture.image() {
                cache
                    .create_view_from_image(image, vk::Format::B8G8R8A8_UNORM, 64, 64)
                    .expect("view creation failed");
            }
            // texture drops here; the cache still holds the VkImageView
        }

        // flush must not panic and must leave the cache empty
        cache.flush();

        let count = cache.view_cache.lock().unwrap().len();
        assert_eq!(count, 0, "cache must be empty after flush");
    }
}
