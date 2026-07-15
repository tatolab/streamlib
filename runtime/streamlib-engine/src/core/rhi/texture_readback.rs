// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor + error types for the host-side texture-readback RHI primitive.
//!
//! Pattern follows production engines (Unreal `FRHIGPUTextureReadback`, bgfx
//! `bgfx::readTexture`, WebGPU `copyTextureToBuffer + mapAsync`, Granite
//! `copy_image_to_buffer + vkSemaphoreWaitKHR`): caller creates a readback
//! handle bound to a fixed format/extent, then submits copies that return a
//! ticket; the staging buffer + command resources + timeline semaphore are
//! allocated once at construction and reused across submits.

use std::ffi::c_void;

use streamlib_plugin_abi::{GpuContextFullAccessVTable, VulkanTextureReadbackMethodsVTable};

use crate::core::rhi::{Texture, TextureFormat};
use crate::core::{Error, Result};

/// Texture-readback descriptor: pin format + extent at construction.
///
/// The handle's staging buffer is sized to `width * height *
/// format.bytes_per_pixel()` and reused across every submit. Submits
/// against a texture whose format/extent disagree with the descriptor
/// return [`TextureReadbackError::DescriptorMismatch`].
#[derive(Debug, Clone)]
pub struct TextureReadbackDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// Pixel format of the texture(s) this readback will copy from.
    pub format: TextureFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Last-known image layout the texture is in when handed to the
/// readback handle's `submit`.
///
/// The readback transitions `source_layout → TRANSFER_SRC_OPTIMAL` for
/// the copy and back to `source_layout` afterward, so the texture is in
/// the same layout before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureSourceLayout {
    /// Image was last used in compute / general read-write
    /// (`VK_IMAGE_LAYOUT_GENERAL`). The default for textures produced by
    /// compute kernels and for images coming back from graphics-API
    /// composers (Skia, OpenGL) that don't expose an explicit layout.
    General,
    /// Image was last used as a color attachment in a render pass
    /// (`VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL`).
    ColorAttachment,
    /// Image was last used as a shader-read-only sampled texture
    /// (`VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL`).
    ShaderReadOnly,
}

impl TextureSourceLayout {
    /// Encode to the raw `VkImageLayout` `i32` that crosses the
    /// readback methods vtable's `source_layout_raw` slot. Uses the
    /// [`streamlib_consumer_rhi::VulkanLayout`] constants (the single
    /// source of truth for the raw enumerant values) so a drift in
    /// vulkanalia's `as_raw()` mapping is caught by that crate's layout
    /// test, not silently re-mapped here.
    pub fn to_vulkan_layout_raw(self) -> i32 {
        use crate::core::rhi::VulkanLayout;
        match self {
            Self::General => VulkanLayout::GENERAL.0,
            Self::ColorAttachment => VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0,
            Self::ShaderReadOnly => VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
        }
    }

    /// Decode from a raw `VkImageLayout` `i32`. Returns `None` for any
    /// layout outside the readback-supported set — the host maps that
    /// to a typed unsupported-layout error rather than transitioning
    /// from a layout the copy path can't restore.
    pub fn from_vulkan_layout_raw(raw: i32) -> Option<Self> {
        use crate::core::rhi::VulkanLayout;
        if raw == VulkanLayout::GENERAL.0 {
            Some(Self::General)
        } else if raw == VulkanLayout::COLOR_ATTACHMENT_OPTIMAL.0 {
            Some(Self::ColorAttachment)
        } else if raw == VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0 {
            Some(Self::ShaderReadOnly)
        } else {
            None
        }
    }
}

/// Opaque ticket returned by submit; pass to `try_read` / `wait_and_read`
/// to retrieve the staging buffer's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadbackTicket {
    /// Identity of the readback handle that issued the ticket. Tickets
    /// from one handle are not valid against another.
    pub(crate) handle_id: u64,
    /// Timeline counter value the handle's signal semaphore reaches when
    /// the GPU copy is complete.
    pub(crate) counter: u64,
}

/// Error taxonomy for the texture-readback RHI primitive.
///
/// Named variants — each carries enough context to diagnose without
/// re-running. `From<TextureReadbackError> for Error` (in the
/// host-side impl module) keeps callers in the existing `Result<T>`
/// channel.
#[derive(Debug, thiserror::Error)]
pub enum TextureReadbackError {
    /// Failed to create the staging buffer or its memory.
    #[error("texture readback '{label}': failed to create staging buffer ({size} bytes): {cause}")]
    StagingBufferAlloc {
        label: String,
        size: u64,
        cause: String,
    },
    /// Failed to create the command pool, command buffer, or fence/semaphore.
    #[error("texture readback '{label}': failed to create command resource ({what}): {cause}")]
    CommandResources {
        label: String,
        what: &'static str,
        cause: String,
    },
    /// Texture handed to submit() doesn't match the descriptor's
    /// format/extent.
    #[error(
        "texture readback '{label}': texture (format={actual_format:?}, {actual_width}x{actual_height}) \
         does not match descriptor (format={expected_format:?}, {expected_width}x{expected_height})"
    )]
    DescriptorMismatch {
        label: String,
        expected_format: TextureFormat,
        expected_width: u32,
        expected_height: u32,
        actual_format: TextureFormat,
        actual_width: u32,
        actual_height: u32,
    },
    /// Texture handed to submit() has no Vulkan image handle (e.g. the
    /// texture was constructed for a non-Vulkan backend).
    #[error("texture readback '{label}': texture has no Vulkan image handle")]
    TextureMissingVulkanImage { label: String },
    /// submit() called while a prior submit's ticket hasn't been waited.
    /// The handle is single-in-flight: hold N handles for N parallel
    /// readbacks, mirroring `VulkanComputeKernel`.
    #[error(
        "texture readback '{label}': submit() called with prior ticket (counter={pending}) still in flight"
    )]
    InFlight { label: String, pending: u64 },
    /// try_read / wait_and_read called with no in-flight submission.
    #[error("texture readback '{label}': no in-flight submission to read from")]
    NoSubmission { label: String },
    /// Ticket from a different readback handle was passed.
    #[error(
        "texture readback '{label}': ticket from foreign handle (id {ticket_handle_id}, this handle id {handle_id})"
    )]
    ForeignTicket {
        label: String,
        handle_id: u64,
        ticket_handle_id: u64,
    },
    /// Ticket counter doesn't match the in-flight submission (caller
    /// passed a ticket from a different submit on the same handle —
    /// since the handle is single-in-flight this means a ticket was
    /// reused across submits).
    #[error(
        "texture readback '{label}': ticket counter {ticket} does not match in-flight counter {pending}"
    )]
    StaleTicket {
        label: String,
        ticket: u64,
        pending: u64,
    },
    /// Command recording or queue submission failed.
    #[error("texture readback '{label}': {what} failed: {cause}")]
    Submit {
        label: String,
        what: &'static str,
        cause: String,
    },
    /// Wait timed out (`wait_and_read` with explicit timeout).
    #[error("texture readback '{label}': wait timed out after {timeout_ns}ns")]
    WaitTimeout { label: String, timeout_ns: u64 },
}

impl TextureReadbackDescriptor<'_> {
    /// Total staging-buffer size in bytes for this descriptor.
    pub fn staging_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.format.bytes_per_pixel() as u64)
    }
}

// =============================================================================
// PluginAbiObject twin
// =============================================================================

/// Single-in-flight GPU→CPU texture readback, exposed across the plugin
/// ABI as a layout-stable `#[repr(C)]` PluginAbiObject so cdylibs can
/// hold, drop, and drive it without sharing rustc-version or dep-graph
/// with the host.
///
/// The opaque `handle` points at a host-owned
/// `Box<Arc<crate::vulkan::rhi::VulkanTextureReadback>>`. Unlike the
/// Arc-into-raw kernel PluginAbiObjects, this one **deviates** from the
/// clone/drop pair convention: the handle is Box-shaped and the object
/// is `!Clone` (the primitive owns exclusive single-in-flight staging +
/// command resources), so the parent [`GpuContextFullAccessVTable`]
/// carries `drop_texture_readback` only — there is no clone slot.
///
/// Per-method dispatch (`submit` / `try_read` / `wait_and_read` /
/// `try_read_copy` / `wait_and_copy`) is reached through the per-type
/// [`VulkanTextureReadbackMethodsVTable`] pointed at by `methods_vtable`.
/// The five POD getters (`width` / `height` / `format` / `handle_id` /
/// `staging_size`) read cached fields directly — no plugin ABI hop.
#[repr(C)]
pub struct TextureReadback {
    /// Opaque handle to the host's
    /// `Box<Arc<crate::vulkan::rhi::VulkanTextureReadback>>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI drop dispatch (`drop_texture_readback`).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const VulkanTextureReadbackMethodsVTable,
    /// Cached process-unique readback handle id. Foreign-ticket checks
    /// run host-side; this is exposed for diagnostics/logging only.
    pub(crate) cached_handle_id: u64,
    /// Cached total staging-buffer size in bytes (`width × height ×
    /// bytes_per_pixel`). Never recomputed ABI-side — sourced from the
    /// primitive's own `staging_size()`.
    pub(crate) cached_staging_size: u64,
    /// Cached pixel width the readback is bound to.
    pub(crate) cached_width: u32,
    /// Cached pixel height the readback is bound to.
    pub(crate) cached_height: u32,
    /// Cached pixel format (plugin-ABI-stable `u32` discriminant,
    /// matches [`TextureFormat`]'s `#[repr(u32)]`).
    pub(crate) cached_format_raw: u32,
    /// Reserved padding (keeps size a multiple of 8; zero, never read).
    pub(crate) _reserved_padding: u32,
}

// SAFETY: `handle` points at a host-owned
// `Box<Arc<VulkanTextureReadback>>` whose inner is Send + Sync (staging
// + command resources serialized by the primitive's own state mutex and
// owned timeline semaphore). The vtable pointers are `&'static`.
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

    /// Pixel width the readback is bound to. Cached POD — no plugin ABI hop.
    pub fn width(&self) -> u32 {
        self.cached_width
    }

    /// Pixel height the readback is bound to. Cached POD — no plugin ABI hop.
    pub fn height(&self) -> u32 {
        self.cached_height
    }

    /// Pixel format the readback is bound to. Cached POD — no plugin ABI
    /// hop. Decoded from the plugin-ABI-stable `u32` discriminant.
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
    /// the prior ticket is read returns an error. Dispatches through the
    /// per-type methods vtable's `submit` slot.
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
        // with it at mint time. `texture.handle` is the borrowed
        // `Texture` PluginAbiObject handle (make-borrow convention); the
        // host reads its cached dimensions for descriptor validation.
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

    /// Non-blocking poll. `Ok(Some(bytes))` once the copy completes
    /// (the slice borrows the host persistent-mapped staging, row
    /// stride = `width × bytes_per_pixel`, no padding — valid only until
    /// the next `submit` on this handle), `Ok(None)` while in flight.
    /// Dispatches through the methods vtable's `try_read` slot.
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
        // SAFETY: on ready, `out_bytes_ptr` borrows the host's
        // persistent-mapped staging; the borrow's lifetime ties to
        // `&self` (valid until the next `submit`, per the slot contract).
        let bytes = unsafe { std::slice::from_raw_parts(out_bytes_ptr, out_len) };
        Ok(Some(bytes))
    }

    /// Block until the copy completes (`timeout_ns == u64::MAX` = no
    /// timeout), then borrow the staging buffer. Same borrow-window
    /// contract as [`Self::try_read`]. Dispatches through the methods
    /// vtable's `wait_and_read` slot.
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
    /// `Vec<u8>` once ready (for callers that must outlive the handle's
    /// borrow window). `Ok(Some(bytes))` when copied, `Ok(None)` while
    /// in flight. Dispatches through the methods vtable's `try_read_copy`
    /// slot; a `status 2` (`out_buf` too small) grows the buffer to the
    /// host-reported length and retries once.
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
            // out_buf too small — grow to the host-reported required
            // length (in out_len) and retry once.
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
    /// an owned `Vec<u8>`. Dispatches through the methods vtable's
    /// `wait_and_copy` slot; `status 2` grows + retries as in
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
            // time. `!Clone`, so no clone bumps to balance — a single
            // drop reclaims the boxed `Arc<VulkanTextureReadback>`.
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
mod texture_readback_pluginabiobject_layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_readback_layout() {
        // PluginAbiObject struct — the engine↔SDK twin (both arms carry
        // a byte-identical `#[repr(C)]` copy; this test + the SDK's
        // matching one are the drift lock):
        //   handle              @ 0  (8 bytes, *const c_void)
        //   vtable              @ 8  (8 bytes, *const GpuContextFullAccessVTable)
        //   methods_vtable      @ 16 (8 bytes, *const VulkanTextureReadbackMethodsVTable)
        //   cached_handle_id    @ 24 (8 bytes, u64)
        //   cached_staging_size @ 32 (8 bytes, u64)
        //   cached_width        @ 40 (4 bytes, u32)
        //   cached_height       @ 44 (4 bytes, u32)
        //   cached_format_raw   @ 48 (4 bytes, u32)
        //   _reserved_padding   @ 52 (4 bytes, u32)
        // Total = 56, align = 8.
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
    fn texture_source_layout_raw_round_trips() {
        for layout in [
            TextureSourceLayout::General,
            TextureSourceLayout::ColorAttachment,
            TextureSourceLayout::ShaderReadOnly,
        ] {
            let raw = layout.to_vulkan_layout_raw();
            assert_eq!(TextureSourceLayout::from_vulkan_layout_raw(raw), Some(layout));
        }
        // UNDEFINED (0) / TRANSFER_SRC_OPTIMAL (6) are outside the
        // supported set — the host maps these to a typed error.
        assert_eq!(TextureSourceLayout::from_vulkan_layout_raw(0), None);
        assert_eq!(TextureSourceLayout::from_vulkan_layout_raw(9999), None);
    }
}
