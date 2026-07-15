// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`TextureReadback`] PluginAbiObject.
//!
//! Single-in-flight GPU→CPU texture readback. Construction is privileged
//! (host-side, via
//! [`crate::context::GpuContextFullAccess::create_texture_readback`]);
//! `submit` / `try_read` / `wait_and_read` / `try_read_copy` /
//! `wait_and_copy` dispatch through the per-type
//! [`streamlib_plugin_abi::VulkanTextureReadbackMethodsVTable`]. The five
//! POD getters read cached fields directly — no plugin ABI hop.
//!
//! Layout-stable `#[repr(C)]`: byte-identical to the engine twin
//! (`core/rhi/texture_readback.rs`). The paired `offset_of!` layout
//! tests in each arm are the drift lock — the engine-free SDK cannot
//! import the engine's copy.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{GpuContextFullAccessVTable, VulkanTextureReadbackMethodsVTable};

use streamlib_consumer_rhi::{TextureFormat, VulkanLayout};

use crate::rhi::Texture;

/// Last-known image layout the texture is in when handed to
/// [`TextureReadback::submit`]. The readback transitions
/// `source_layout → TRANSFER_SRC_OPTIMAL` for the copy and back
/// afterward, so the texture is in the same layout before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureSourceLayout {
    /// `VK_IMAGE_LAYOUT_GENERAL` — the default for compute-produced
    /// textures and images coming back from graphics-API composers.
    General,
    /// `VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL`.
    ColorAttachment,
    /// `VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL`.
    ShaderReadOnly,
}

impl TextureSourceLayout {
    /// Encode to the raw `VkImageLayout` `i32` that crosses the readback
    /// methods vtable's `source_layout_raw` slot (via the
    /// [`VulkanLayout`] constants — the single source of truth).
    pub fn to_vulkan_layout_raw(self) -> i32 {
        match self {
            Self::General => VulkanLayout::GENERAL.0,
            Self::ColorAttachment => VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0,
            Self::ShaderReadOnly => VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
        }
    }
}

/// Opaque ticket returned by [`TextureReadback::submit`]; pass to
/// `try_read` / `wait_and_read` / `try_read_copy` / `wait_and_copy` to
/// retrieve the staging bytes. Crosses the plugin ABI as two bare `u64`
/// (`handle_id`, `counter`), preserving the host's foreign/stale-ticket
/// identity checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadbackTicket {
    pub(crate) handle_id: u64,
    pub(crate) counter: u64,
}

/// Single-in-flight GPU→CPU texture readback (cdylib arm).
///
/// Layout-stable `#[repr(C)]` PluginAbiObject. `!Clone` (the primitive
/// owns exclusive single-in-flight resources): the parent
/// [`GpuContextFullAccessVTable`] carries `drop_texture_readback` only —
/// no clone slot. Per-method dispatch goes through the per-type
/// [`VulkanTextureReadbackMethodsVTable`]. The five POD getters read
/// cached fields directly.
#[repr(C)]
pub struct TextureReadback {
    /// Opaque handle to the host's `Box<Arc<VulkanTextureReadback>>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI drop dispatch (`drop_texture_readback`).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const VulkanTextureReadbackMethodsVTable,
    /// Cached process-unique readback handle id.
    pub(crate) cached_handle_id: u64,
    /// Cached total staging-buffer size in bytes.
    pub(crate) cached_staging_size: u64,
    /// Cached pixel width the readback is bound to.
    pub(crate) cached_width: u32,
    /// Cached pixel height the readback is bound to.
    pub(crate) cached_height: u32,
    /// Cached pixel format (plugin-ABI-stable `u32` discriminant).
    pub(crate) cached_format_raw: u32,
    /// Reserved padding (zero, never read).
    pub(crate) _reserved_padding: u32,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<VulkanTextureReadback>>`
// whose inner is Send + Sync; the vtable pointers are `&'static`.
unsafe impl Send for TextureReadback {}
unsafe impl Sync for TextureReadback {}

impl TextureReadback {
    /// Process-unique readback handle id. Cached POD — no plugin ABI hop.
    pub fn handle_id(&self) -> u64 {
        self.cached_handle_id
    }

    /// Total staging-buffer size in bytes. Cached POD — no plugin ABI hop.
    pub fn staging_size(&self) -> u64 {
        self.cached_staging_size
    }

    /// Pixel width the readback is bound to. Cached POD.
    pub fn width(&self) -> u32 {
        self.cached_width
    }

    /// Pixel height the readback is bound to. Cached POD.
    pub fn height(&self) -> u32 {
        self.cached_height
    }

    /// Pixel format the readback is bound to. Cached POD — decoded from
    /// the plugin-ABI-stable `u32` discriminant.
    pub fn format(&self) -> TextureFormat {
        match self.cached_format_raw {
            0 => TextureFormat::Rgba8Unorm,
            1 => TextureFormat::Rgba8UnormSrgb,
            2 => TextureFormat::Bgra8Unorm,
            3 => TextureFormat::Bgra8UnormSrgb,
            4 => TextureFormat::Rgba16Float,
            5 => TextureFormat::Rgba32Float,
            6 => TextureFormat::Nv12,
            _ => TextureFormat::Rgba8Unorm,
        }
    }

    /// Schedule a GPU→CPU copy of `texture` (at its current
    /// `source_layout`) into the readback's staging buffer, returning a
    /// [`ReadbackTicket`]. Single-in-flight: a second `submit` before
    /// the prior ticket is read errors. Dispatches through the methods
    /// vtable's `submit` slot.
    pub fn submit(
        &self,
        texture: &Texture,
        source_layout: TextureSourceLayout,
    ) -> Result<ReadbackTicket> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit: texture-readback methods vtable is null".into(),
            ));
        }
        let mut out_handle_id: u64 = 0;
        let mut out_counter: u64 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; handle paired
        // with it at mint time. `texture.handle` is the borrowed Texture
        // PluginAbiObject handle (make-borrow convention host-side).
        let status = unsafe {
            ((*self.methods_vtable).submit)(
                self.handle,
                texture.handle,
                source_layout.to_vulkan_layout_raw(),
                &mut out_handle_id as *mut u64,
                &mut out_counter as *mut u64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        Ok(ReadbackTicket {
            handle_id: out_handle_id,
            counter: out_counter,
        })
    }

    /// Non-blocking poll. `Ok(Some(bytes))` once the copy completes (the
    /// slice borrows the host persistent-mapped staging, row stride =
    /// `width × bytes_per_pixel`, no padding — valid only until the next
    /// `submit` on this handle), `Ok(None)` while in flight.
    pub fn try_read(&self, ticket: ReadbackTicket) -> Result<Option<&[u8]>> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "try_read: texture-readback methods vtable is null".into(),
            ));
        }
        let mut out_ready: u32 = 0;
        let mut out_bytes_ptr: *const u8 = std::ptr::null();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; out-params point
        // at owned stack storage the host writes on success.
        let status = unsafe {
            ((*self.methods_vtable).try_read)(
                self.handle,
                ticket.handle_id,
                ticket.counter,
                &mut out_ready as *mut u32,
                &mut out_bytes_ptr as *mut *const u8,
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_ready == 0 {
            return Ok(None);
        }
        // SAFETY: on ready, `out_bytes_ptr` borrows the host persistent-
        // mapped staging; the borrow's lifetime ties to `&self`.
        let bytes = unsafe { std::slice::from_raw_parts(out_bytes_ptr, out_len) };
        Ok(Some(bytes))
    }

    /// Block until the copy completes (`timeout_ns == u64::MAX` = no
    /// timeout), then borrow the staging buffer. Same borrow-window
    /// contract as [`Self::try_read`].
    pub fn wait_and_read(&self, ticket: ReadbackTicket, timeout_ns: u64) -> Result<&[u8]> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "wait_and_read: texture-readback methods vtable is null".into(),
            ));
        }
        let mut out_bytes_ptr: *const u8 = std::ptr::null();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; out-params point
        // at owned stack storage the host writes on success.
        let status = unsafe {
            ((*self.methods_vtable).wait_and_read)(
                self.handle,
                ticket.handle_id,
                ticket.counter,
                timeout_ns,
                &mut out_bytes_ptr as *mut *const u8,
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        // SAFETY: `out_bytes_ptr` borrows the host persistent-mapped
        // staging; lifetime tied to `&self` (valid until next `submit`).
        let bytes = unsafe { std::slice::from_raw_parts(out_bytes_ptr, out_len) };
        Ok(bytes)
    }

    /// Non-blocking poll that COPIES the staging bytes into an owned
    /// `Vec<u8>` once ready (for callers that must outlive the borrow
    /// window). `Ok(Some(bytes))` when copied, `Ok(None)` while in
    /// flight. `status 2` (`out_buf` too small) grows to the host-
    /// reported length and retries once.
    pub fn try_read_copy(&self, ticket: ReadbackTicket) -> Result<Option<Vec<u8>>> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "try_read_copy: texture-readback methods vtable is null".into(),
            ));
        }
        let mut out = vec![0u8; self.cached_staging_size as usize];
        let mut out_ready: u32 = 0;
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; `out` owns
        // `out.len()` writable bytes the host copies into on ready.
        let status = unsafe {
            ((*self.methods_vtable).try_read_copy)(
                self.handle,
                ticket.handle_id,
                ticket.counter,
                &mut out_ready as *mut u32,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 2 {
            out = vec![0u8; out_len];
            out_ready = 0;
            out_len = 0;
            let retry = unsafe {
                ((*self.methods_vtable).try_read_copy)(
                    self.handle,
                    ticket.handle_id,
                    ticket.counter,
                    &mut out_ready as *mut u32,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut out_len as *mut usize,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            };
            if retry != 0 {
                let msg =
                    String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
                return Err(Error::GpuError(msg));
            }
        } else if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_ready == 0 {
            return Ok(None);
        }
        out.truncate(out_len);
        Ok(Some(out))
    }

    /// Block until the copy completes, then COPY the staging bytes into
    /// an owned `Vec<u8>`. `status 2` grows + retries as in
    /// [`Self::try_read_copy`].
    pub fn wait_and_copy(&self, ticket: ReadbackTicket, timeout_ns: u64) -> Result<Vec<u8>> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "wait_and_copy: texture-readback methods vtable is null".into(),
            ));
        }
        let mut out = vec![0u8; self.cached_staging_size as usize];
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; `out` owns
        // `out.len()` writable bytes the host copies into on success.
        let status = unsafe {
            ((*self.methods_vtable).wait_and_copy)(
                self.handle,
                ticket.handle_id,
                ticket.counter,
                timeout_ns,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 2 {
            out = vec![0u8; out_len];
            out_len = 0;
            let retry = unsafe {
                ((*self.methods_vtable).wait_and_copy)(
                    self.handle,
                    ticket.handle_id,
                    ticket.counter,
                    timeout_ns,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut out_len as *mut usize,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            };
            if retry != 0 {
                let msg =
                    String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
                return Err(Error::GpuError(msg));
            }
        } else if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        out.truncate(out_len);
        Ok(out)
    }
}

impl Drop for TextureReadback {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Box::into_raw` at mint
            // time. `!Clone`, so a single drop reclaims the boxed
            // `Arc<VulkanTextureReadback>`.
            unsafe {
                ((*self.vtable).drop_texture_readback)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for TextureReadback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureReadback")
            .field("handle_id", &self.cached_handle_id)
            .field("width", &self.cached_width)
            .field("height", &self.cached_height)
            .field("format", &self.format())
            .field("staging_size", &self.cached_staging_size)
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_readback_layout() {
        // Must match the engine's
        // `core/rhi/texture_readback.rs::TextureReadback`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16,
        //   cached_handle_id @ 24, cached_staging_size @ 32,
        //   cached_width @ 40, cached_height @ 44, cached_format_raw @ 48,
        //   _reserved_padding @ 52. Total 56 bytes, align 8.
        assert_eq!(size_of::<TextureReadback>(), 56);
        assert_eq!(align_of::<TextureReadback>(), 8);
        assert_eq!(offset_of!(TextureReadback, handle), 0);
        assert_eq!(offset_of!(TextureReadback, vtable), 8);
        assert_eq!(offset_of!(TextureReadback, methods_vtable), 16);
        assert_eq!(offset_of!(TextureReadback, cached_handle_id), 24);
        assert_eq!(offset_of!(TextureReadback, cached_staging_size), 32);
        assert_eq!(offset_of!(TextureReadback, cached_width), 40);
        assert_eq!(offset_of!(TextureReadback, cached_height), 44);
        assert_eq!(offset_of!(TextureReadback, cached_format_raw), 48);
        assert_eq!(offset_of!(TextureReadback, _reserved_padding), 52);
    }

    #[test]
    fn texture_readback_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TextureReadback>();
    }

    #[test]
    fn texture_source_layout_raw_matches_vulkan_layout() {
        assert_eq!(
            TextureSourceLayout::General.to_vulkan_layout_raw(),
            VulkanLayout::GENERAL.0
        );
        assert_eq!(
            TextureSourceLayout::ColorAttachment.to_vulkan_layout_raw(),
            VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0
        );
        assert_eq!(
            TextureSourceLayout::ShaderReadOnly.to_vulkan_layout_raw(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0
        );
    }
}
