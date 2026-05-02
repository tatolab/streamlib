// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-wide per-surface texture registration record.
//!
//! Stored in [`crate::core::context::GpuContext`]'s same-process texture
//! cache, keyed by `surface_id`. Mirrors the per-surface state pattern
//! the surface adapters already use (see
//! `streamlib-adapter-vulkan::SurfaceState::current_layout`) but lifted
//! to the engine-wide cache so consumers reaching textures via
//! `resolve_videoframe_registration` get the same lifecycle metadata
//! adapter consumers do.
//!
//! On Linux the registration carries the texture's last-known
//! `VkImageLayout` so consumers can issue a correct
//! `vkCmdPipelineBarrier2` source layout. On other platforms only the
//! texture is held — Metal manages texture state automatically and
//! Vulkan layouts don't apply.

use std::sync::Arc;

use crate::core::rhi::StreamTexture;

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicI32, Ordering};
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

/// Per-surface registration record held by [`crate::core::context::GpuContext`]'s
/// texture cache.
pub struct TextureRegistration {
    texture: StreamTexture,
    /// Last-known Vulkan image layout. Producers update after their
    /// final layout transition; consumers read before issuing their
    /// own barrier and update after.
    ///
    /// Multi-consumer races are tolerated: Vulkan barriers are
    /// serialized by the queue mutex, so the GPU work each consumer
    /// submits is correct regardless of which one wins the atomic
    /// update; the field tracks "best-known stable layout for the
    /// next reader."
    #[cfg(target_os = "linux")]
    current_layout: AtomicI32,
}

impl TextureRegistration {
    /// Construct a registration with an initial layout.
    #[cfg(target_os = "linux")]
    pub fn new(texture: StreamTexture, initial_layout: VulkanLayout) -> Arc<Self> {
        Arc::new(Self {
            texture,
            current_layout: AtomicI32::new(initial_layout.0),
        })
    }

    /// Construct a registration on platforms without Vulkan layout tracking.
    #[cfg(not(target_os = "linux"))]
    pub fn new(texture: StreamTexture) -> Arc<Self> {
        Arc::new(Self { texture })
    }

    /// Borrow the underlying texture.
    pub fn texture(&self) -> &StreamTexture {
        &self.texture
    }

    /// Last-known `VkImageLayout` the texture is in.
    #[cfg(target_os = "linux")]
    pub fn current_layout(&self) -> VulkanLayout {
        VulkanLayout(self.current_layout.load(Ordering::Acquire))
    }

    /// Record a new last-known layout.
    #[cfg(target_os = "linux")]
    pub fn update_layout(&self, new_layout: VulkanLayout) {
        self.current_layout.store(new_layout.0, Ordering::Release);
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
    use crate::core::context::GpuContext;
    use std::thread;

    fn fresh_texture() -> Option<StreamTexture> {
        let gpu = GpuContext::init_for_platform().ok()?;
        let desc = TextureDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING);
        gpu.device().create_texture(&desc).ok()
    }

    #[test]
    fn current_layout_round_trip() {
        let Some(texture) = fresh_texture() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let reg = TextureRegistration::new(texture, VulkanLayout::UNDEFINED);
        assert_eq!(reg.current_layout(), VulkanLayout::UNDEFINED);
        reg.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        assert_eq!(reg.current_layout(), VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        reg.update_layout(VulkanLayout::GENERAL);
        assert_eq!(reg.current_layout(), VulkanLayout::GENERAL);
    }

    #[test]
    fn concurrent_updates_dont_tear() {
        let Some(texture) = fresh_texture() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let reg = TextureRegistration::new(texture, VulkanLayout::UNDEFINED);
        let layouts = [
            VulkanLayout::GENERAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
        ];
        let handles: Vec<_> = layouts
            .iter()
            .map(|&layout| {
                let reg = Arc::clone(&reg);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        reg.update_layout(layout);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread join");
        }
        // Final value is one of the written layouts — atomic guarantees no torn reads.
        let final_layout = reg.current_layout();
        assert!(
            layouts.iter().any(|&l| l == final_layout),
            "final layout {:?} is not one of the written values",
            final_layout
        );
    }
}
