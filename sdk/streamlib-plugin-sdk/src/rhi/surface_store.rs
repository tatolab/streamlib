// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's cross-process surface-share handle —
//! the producer registration path (#1260).
//!
//! [`SurfaceStore`] is layout-stable `#[repr(C)] { handle, vtable }`;
//! clone/drop and every method dispatch through the host-installed
//! [`streamlib_plugin_abi::SurfaceStoreVTable`]. The host
//! `SurfaceStoreInner` backing (XPC on macOS, Unix socket on Linux) stays
//! in the engine.
//!
//! Obtained from
//! [`crate::context::GpuContextLimitedAccess::surface_store`] /
//! [`crate::context::GpuContextFullAccess::surface_store`]. The producer
//! path this issue exposes is [`Self::register_texture`]: a cdylib
//! registers its ring textures + per-slot `produce_done` / `consume_done`
//! timeline pairs into surface-share without naming the host
//! `HostVulkanTimelineSemaphore` — it passes
//! [`HostTimelineSemaphore`](crate::rhi::HostTimelineSemaphore) values
//! minted through the FullAccess
//! `create_exportable_timeline_semaphore` slot.

use std::ffi::c_void;

use streamlib_consumer_rhi::VulkanLayout;
use streamlib_error::{Error, Result};
use streamlib_plugin_abi::SurfaceStoreVTable;

use crate::rhi::{HostTimelineSemaphore, PixelBuffer, Texture};

/// Cross-process surface-sharing handle (producer arm).
///
/// A null-handle value is the "None" sentinel returned when the host has
/// no surface store wired; [`Self::is_none`] reports it and every method
/// short-circuits with a typed error.
#[repr(C)]
pub struct SurfaceStore {
    /// Opaque handle to the host's `Arc<SurfaceStoreInner>`.
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI clone/drop + method dispatch.
    pub(crate) vtable: *const SurfaceStoreVTable,
}

// SAFETY: same shape as the engine twin. The handle is a host-owned
// `Arc<SurfaceStoreInner>` (Send+Sync, Mutex-protected); the vtable
// pointer is `&'static` in the host image.
unsafe impl Send for SurfaceStore {}
unsafe impl Sync for SurfaceStore {}

impl SurfaceStore {
    /// Whether this is a null-handle sentinel (the host has no surface
    /// store, or the accessor returned "None").
    pub fn is_none(&self) -> bool {
        self.handle.is_null() || self.vtable.is_null()
    }

    /// Connect to the surface-share service (XPC on macOS, Unix socket on
    /// Linux). Dispatches through the vtable's `connect` slot. Idempotent
    /// when the underlying store is already connected.
    pub fn connect(&self) -> Result<()> {
        if self.is_none() {
            return Err(Error::GpuError("SurfaceStore::connect: null handle".into()));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired by the host when it wrote this
        // PluginAbiObject into the accessor's out-param.
        let status = unsafe {
            ((*self.vtable).connect)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit("connect", status, &err_buf, err_len)
    }

    /// Disconnect from the surface-share service. Dispatches through the
    /// vtable's `disconnect` slot.
    pub fn disconnect(&self) -> Result<()> {
        if self.is_none() {
            return Err(Error::GpuError(
                "SurfaceStore::disconnect: null handle".into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction.
        let status = unsafe {
            ((*self.vtable).disconnect)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit("disconnect", status, &err_buf, err_len)
    }

    /// Register a texture for cross-process sharing under `surface_id`,
    /// with optional `produce_done` / `consume_done` timeline sidecars
    /// (single-writer-per-edge; see
    /// `docs/architecture/adapter-timeline-single-writer.md`). Dispatches
    /// through the vtable's `register_texture` slot (Linux-only host-side).
    ///
    /// The cdylib passes a pointer to its own [`Texture`] PluginAbiObject
    /// (the host reinterprets the layout-identical bytes) and the inner
    /// handles of the exportable timelines; the host derefs the timeline
    /// handles as `Arc<HostVulkanTimelineSemaphore>` borrows.
    ///
    /// Lifetime contract: registration only BORROWS the `produce_done` /
    /// `consume_done` timelines — the host exports each one's `OPAQUE_FD`
    /// during this call and retains no clone of the
    /// [`HostTimelineSemaphore`](crate::rhi::HostTimelineSemaphore). The
    /// caller MUST keep both timeline values alive (and keep signalling
    /// `produce_done` by GPU-queue completion) for the whole
    /// registration / session lifetime. Dropping a timeline after
    /// registration decrements the host `VkSemaphore` refcount to zero, so
    /// the producer can never signal `produce_done` again and a subprocess
    /// consumer blocks forever waiting on it.
    pub fn register_texture(
        &self,
        surface_id: &str,
        texture: &Texture,
        produce_done: Option<&HostTimelineSemaphore>,
        consume_done: Option<&HostTimelineSemaphore>,
        layout: VulkanLayout,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::GpuError(
                "SurfaceStore::register_texture: null handle".into(),
            ));
        }
        let produce_done_ptr = produce_done
            .map(|t| t.cdylib_handle())
            .unwrap_or(std::ptr::null());
        let consume_done_ptr = consume_done
            .map(|t| t.cdylib_handle())
            .unwrap_or(std::ptr::null());
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; `texture` is a
        // live `&Texture` whose `#[repr(C)]` layout matches the engine's,
        // and the timeline handles are the host-minted inner Arc pointers.
        let status = unsafe {
            ((*self.vtable).register_texture)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                texture as *const Texture as *const c_void,
                produce_done_ptr,
                consume_done_ptr,
                layout.0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit("register_texture", status, &err_buf, err_len)
    }

    /// PixelBuffer sibling of [`Self::register_texture`]: register a pixel
    /// buffer for cross-process sharing under `surface_id`, with optional
    /// `produce_done` / `consume_done` timeline sidecars
    /// (single-writer-per-edge; see
    /// `docs/architecture/adapter-timeline-single-writer.md`). Dispatches
    /// through the vtable's `register_pixel_buffer_with_timeline` slot
    /// (Linux-only host-side).
    ///
    /// The cdylib passes a pointer to its own [`PixelBuffer`] PluginAbiObject
    /// (the host reinterprets the layout-identical bytes) and the inner
    /// handles of the exportable timelines; the host derefs the timeline
    /// handles as `Arc<HostVulkanTimelineSemaphore>` borrows.
    ///
    /// Unlike [`Self::register_texture`], this slot takes no `VkImageLayout`
    /// — a flat pixel-buffer allocation carries no image-layout state.
    ///
    /// Lifetime contract matches [`Self::register_texture`]: registration
    /// only BORROWS the `produce_done` / `consume_done` timelines — the host
    /// exports each one's `OPAQUE_FD` during this call and retains no clone
    /// of the [`HostTimelineSemaphore`]. The caller MUST keep both timeline
    /// values alive (and keep signalling `produce_done` by GPU-queue
    /// completion) for the whole registration / session lifetime. Dropping a
    /// timeline after registration decrements the host `VkSemaphore` refcount
    /// to zero, so the producer can never signal `produce_done` again and a
    /// subprocess consumer blocks forever waiting on it.
    pub fn register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        produce_done: Option<&HostTimelineSemaphore>,
        consume_done: Option<&HostTimelineSemaphore>,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::GpuError(
                "SurfaceStore::register_pixel_buffer_with_timeline: null handle".into(),
            ));
        }
        let produce_done_ptr = produce_done
            .map(|t| t.cdylib_handle())
            .unwrap_or(std::ptr::null());
        let consume_done_ptr = consume_done
            .map(|t| t.cdylib_handle())
            .unwrap_or(std::ptr::null());
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction; `pixel_buffer` is a
        // live `&PixelBuffer` whose `#[repr(C)]` layout matches the engine's,
        // and the timeline handles are the host-minted inner Arc pointers.
        let status = unsafe {
            ((*self.vtable).register_pixel_buffer_with_timeline)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                pixel_buffer as *const PixelBuffer as *const c_void,
                produce_done_ptr,
                consume_done_ptr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit(
            "register_pixel_buffer_with_timeline",
            status,
            &err_buf,
            err_len,
        )
    }

    /// Update the published `VkImageLayout` for an already-registered
    /// texture. Producer-side per-frame op after a layout transition.
    /// Dispatches through the vtable's `update_image_layout` slot.
    pub fn update_image_layout(&self, surface_id: &str, layout: VulkanLayout) -> Result<()> {
        if self.is_none() {
            return Err(Error::GpuError(
                "SurfaceStore::update_image_layout: null handle".into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable paired at construction.
        let status = unsafe {
            ((*self.vtable).update_image_layout)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                layout.0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit("update_image_layout", status, &err_buf, err_len)
    }
}

fn status_to_unit(op: &str, status: i32, err_buf: &[u8], err_len: usize) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
        Err(Error::GpuError(format!("SurfaceStore::{op}: {msg}")))
    }
}

impl Clone for SurfaceStore {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle + vtable paired at construction; the vtable's
            // `clone_handle` contract is `Arc::increment_strong_count`.
            unsafe {
                ((*self.vtable).clone_handle)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for SurfaceStore {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_handle` bumps.
            unsafe {
                ((*self.vtable).drop_handle)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for SurfaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceStore")
            .field("is_none", &self.is_none())
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn surface_store_layout() {
        // Must match the engine's
        // `core/context/surface_store.rs::SurfaceStore`:
        //   handle @ 0, vtable @ 8. Total 16 bytes, align 8.
        assert_eq!(size_of::<SurfaceStore>(), 16);
        assert_eq!(align_of::<SurfaceStore>(), 8);
        assert_eq!(offset_of!(SurfaceStore, handle), 0);
        assert_eq!(offset_of!(SurfaceStore, vtable), 8);
    }

    #[test]
    fn surface_store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SurfaceStore>();
    }

    #[test]
    fn null_store_methods_are_typed_errors_not_ub() {
        // Mental-revert: drop the `is_none()` guards and each dispatch
        // UB-derefs a null vtable pointer (SIGSEGV).
        let store = SurfaceStore {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        };
        assert!(store.is_none());
        assert!(store.connect().is_err());
        assert!(store.disconnect().is_err());
        assert!(store.update_image_layout("s", VulkanLayout(0)).is_err());
        // Null store + null timelines: register_texture must refuse
        // before any dispatch. Build a null-handle Texture to pass — the
        // is_none() guard fires before the texture pointer or the vtable
        // is ever read, so the null envelope is never dereferenced.
        //
        // Mental-revert: drop register_texture's is_none() guard and this
        // call UB-derefs the null `*const SurfaceStoreVTable` (SIGSEGV).
        let null_texture = Texture {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            width_cached: 0,
            height_cached: 0,
            format_raw: 0,
            _padding: 0,
        };
        assert!(
            store
                .register_texture("s", &null_texture, None, None, VulkanLayout(0))
                .is_err(),
            "register_texture on a null store must return a typed Err, not UB"
        );
        // Same guard for the PixelBuffer sibling: build a null-handle
        // PixelBuffer to pass — the is_none() guard fires before the pixel
        // buffer pointer or the vtable is ever read.
        //
        // Mental-revert: drop register_pixel_buffer_with_timeline's is_none()
        // guard and this call UB-derefs the null `*const SurfaceStoreVTable`
        // (SIGSEGV).
        let null_pixel_buffer = PixelBuffer {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            width: 0,
            height: 0,
            format_raw: 0,
            plane_count_cached: 0,
        };
        assert!(
            store
                .register_pixel_buffer_with_timeline("s", &null_pixel_buffer, None, None)
                .is_err(),
            "register_pixel_buffer_with_timeline on a null store must return a typed Err, not UB"
        );
        drop(store);
    }
}
