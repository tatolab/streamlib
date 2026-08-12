// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-wide per-surface texture registration record.
//!
//! Layout-stable `(handle, vtable)` shape so the type crosses the
//! plugin ABI. The handle is
//! `Arc::into_raw(Arc<TextureRegistrationInner>)`; the vtable's
//! `clone_texture_registration` / `drop_texture_registration`
//! callbacks manage the Arc refcount in host-compiled code.
//!
//! Stored in [`crate::core::context::GpuContext`]'s same-process texture
//! cache, keyed by `surface_id`. Mirrors the per-surface state pattern
//! the surface adapters already use (see
//! `streamlib-adapter-vulkan::SurfaceState::current_layout`) but lifted
//! to the engine-wide cache so consumers reaching textures via
//! `resolve_texture_registration_by_surface_id` get the same lifecycle
//! metadata adapter consumers do.
//!
//! On Linux the registration carries the texture's last-known
//! `VkImageLayout` so consumers can issue a correct
//! `vkCmdPipelineBarrier2` source layout. On other platforms only the
//! texture is held — Metal manages texture state automatically and
//! Vulkan layouts don't apply.

use std::ffi::c_void;
use std::sync::Arc;

use crate::core::rhi::Texture;

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicI32, Ordering};
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

/// Rich data backing a [`TextureRegistration`], reached through the
/// registration's opaque handle.
pub(crate) struct TextureRegistrationInner {
    pub(crate) texture: Texture,
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
    pub(crate) current_layout: AtomicI32,
}

/// Per-surface registration record held by
/// [`crate::core::context::GpuContext`]'s texture cache.
///
/// Cheap to clone — semantically `Arc::clone` over the opaque handle.
pub struct TextureRegistration {
    /// Opaque handle to the host's `Arc<TextureRegistrationInner>`
    /// (produced by `Arc::into_raw`).
    pub(crate) handle: *const c_void,
}

// SAFETY: `handle` points at an `Arc<TextureRegistrationInner>` whose
// interior is Send+Sync (Texture is Send+Sync per its own unsafe
// impls; AtomicI32 is Send+Sync by definition).
unsafe impl Send for TextureRegistration {}
unsafe impl Sync for TextureRegistration {}

impl TextureRegistration {
    /// Construct a registration with an initial layout.
    #[cfg(target_os = "linux")]
    pub fn new(texture: Texture, initial_layout: VulkanLayout) -> Self {
        let inner = TextureRegistrationInner {
            texture,
            current_layout: AtomicI32::new(initial_layout.0),
        };
        Self::from_arc_into_raw(Arc::new(inner))
    }

    /// Construct a registration on platforms without Vulkan layout tracking.
    #[cfg(not(target_os = "linux"))]
    pub fn new(texture: Texture) -> Self {
        let inner = TextureRegistrationInner { texture };
        Self::from_arc_into_raw(Arc::new(inner))
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw` and wrap it as the opaque handle.
    pub(crate) fn from_arc_into_raw(arc: Arc<TextureRegistrationInner>) -> Self {
        let handle = Arc::into_raw(arc) as *const c_void;
        Self { handle }
    }

    /// Engine-internal borrow of the host-owned `TextureRegistrationInner`.
    pub(crate) fn host_inner(&self) -> &TextureRegistrationInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`.
        // The leaked strong count keeps the inner alive at least until Drop.
        unsafe { &*(self.handle as *const TextureRegistrationInner) }
    }

    /// Borrow the underlying texture. The returned reference is valid
    /// for the lifetime of `self` — the `Arc` keeps the inner alive.
    pub fn texture(&self) -> &Texture {
        if self.handle.is_null() {
            panic!("TextureRegistration::texture() called on a null-handle registration");
        }
        &self.host_inner().texture
    }

    /// Last-known `VkImageLayout` the texture is in.
    #[cfg(target_os = "linux")]
    pub fn current_layout(&self) -> VulkanLayout {
        if self.handle.is_null() {
            return VulkanLayout::UNDEFINED;
        }
        VulkanLayout(self.host_inner().current_layout.load(Ordering::Acquire))
    }

    /// Record a new last-known layout.
    #[cfg(target_os = "linux")]
    pub fn update_layout(&self, new_layout: VulkanLayout) {
        if self.handle.is_null() {
            return;
        }
        self.host_inner()
            .current_layout
            .store(new_layout.0, Ordering::Release);
    }
}

impl Clone for TextureRegistration {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const TextureRegistrationInner);
            }
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for TextureRegistration {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const TextureRegistrationInner);
            }
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;

    #[test]
    fn texture_registration_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TextureRegistration>();
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::core::context::GpuContext;
    use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
    use std::thread;

    fn fresh_texture() -> Option<Texture> {
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
                let reg = reg.clone();
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
