// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`TextureRegistration`] PluginAbiObject.
//!
//! Layout-stable `#[repr(C)] (handle, vtable)` shape mirroring the engine's
//! `core/context/texture_registration.rs::TextureRegistration`. The host
//! `TextureRegistrationInner` backing + the `new` / `from_arc_into_raw` /
//! `host_inner` constructors stay in the engine; this twin carries only the
//! vtable-dispatched `texture` / `current_layout` / `update_layout` / Clone /
//! Drop methods a cdylib consumer needs after resolving an incoming
//! `surface_id` via
//! [`crate::context::GpuContextLimitedAccess::resolve_texture_registration_by_surface_id`].

use std::ffi::c_void;

use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

use crate::rhi::Texture;

/// Per-surface registration record resolved from a `surface_id` — the
/// texture plus its last-known Vulkan image layout.
///
/// Layout-stable: `#[repr(C)] (handle, vtable)`. Clone bumps the host's
/// `Arc<TextureRegistrationInner>` strong count via
/// [`GpuContextLimitedAccessVTable::clone_texture_registration`]; Drop
/// decrements via [`GpuContextLimitedAccessVTable::drop_texture_registration`].
/// Both run in host-compiled code regardless of the calling plugin.
#[repr(C)]
pub struct TextureRegistration {
    /// Opaque handle to the host's `Arc<TextureRegistrationInner>` (produced
    /// by `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop and method dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at an `Arc<TextureRegistrationInner>` whose interior
// is Send+Sync. Refcount management crosses the plugin ABI through the vtable,
// but the underlying Arc bookkeeping runs in host-compiled code regardless.
unsafe impl Send for TextureRegistration {}
unsafe impl Sync for TextureRegistration {}

impl TextureRegistration {
    /// Borrow the underlying texture.
    ///
    /// Dispatches through the vtable's
    /// [`GpuContextLimitedAccessVTable::texture_registration_texture`]
    /// callback. The returned reference is valid for the lifetime of `self`
    /// — the host's `Arc` keeps the inner alive.
    pub fn texture(&self) -> &Texture {
        if self.handle.is_null() || self.vtable.is_null() {
            panic!("TextureRegistration::texture() called on a null-handle registration");
        }
        // SAFETY: vtable + handle were paired at construction; the callback
        // returns a `*const Texture` (typed `*const c_void` at the plugin
        // ABI) into the Arc's heap allocation, alive as long as `self`.
        // [`Texture`] is a layout-stable `#[repr(C)]` value (locked by the
        // `texture_layout` regression test) so cdylib and host see the same
        // byte shape.
        unsafe {
            let ptr = ((*self.vtable).texture_registration_texture)(self.handle);
            &*(ptr as *const Texture)
        }
    }

    /// Last-known `VkImageLayout` the texture is in.
    ///
    /// Dispatches through the vtable's
    /// [`GpuContextLimitedAccessVTable::texture_registration_current_layout`]
    /// callback.
    #[cfg(target_os = "linux")]
    pub fn current_layout(&self) -> VulkanLayout {
        if self.handle.is_null() || self.vtable.is_null() {
            return VulkanLayout::UNDEFINED;
        }
        // SAFETY: vtable + handle were paired at construction.
        let raw = unsafe { ((*self.vtable).texture_registration_current_layout)(self.handle) };
        VulkanLayout(raw)
    }

    /// Record a new last-known layout after a transition.
    ///
    /// Dispatches through the vtable's
    /// [`GpuContextLimitedAccessVTable::texture_registration_update_layout`]
    /// callback.
    #[cfg(target_os = "linux")]
    pub fn update_layout(&self, new_layout: VulkanLayout) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: vtable + handle were paired at construction.
        unsafe {
            ((*self.vtable).texture_registration_update_layout)(self.handle, new_layout.0);
        }
    }
}

impl Clone for TextureRegistration {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at construction; the vtable's
            // `clone_texture_registration` contract is
            // `Arc::increment_strong_count` host-side. Balanced by Drop.
            unsafe {
                ((*self.vtable).clone_texture_registration)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for TextureRegistration {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_texture_registration` bumps.
            unsafe {
                ((*self.vtable).drop_texture_registration)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for TextureRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureRegistration").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_registration_layout() {
        // Must match the engine's
        // `core/context/texture_registration.rs::TextureRegistration`:
        //   handle @ 0, vtable @ 8. Total 16 bytes, align 8.
        assert_eq!(size_of::<TextureRegistration>(), 16);
        assert_eq!(align_of::<TextureRegistration>(), 8);
        assert_eq!(offset_of!(TextureRegistration, handle), 0);
        assert_eq!(offset_of!(TextureRegistration, vtable), 8);
    }

    #[test]
    fn texture_registration_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TextureRegistration>();
    }
}
