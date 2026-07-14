// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`RhiCommandRecorder`] PluginAbiObject.
//!
//! Layout-stable `#[repr(C)] { handle, vtable, methods_vtable }`. The
//! opaque handle points at the host's `Box<RhiCommandRecorderInner>`;
//! Drop dispatches through the parent
//! [`GpuContextFullAccessVTable`]'s `drop_command_recorder`. Per-method
//! dispatch routes through the per-type
//! [`streamlib_plugin_abi::RhiCommandRecorderMethodsVTable`].
//!
//! **Single-owner; deliberately NOT `Clone`** — recording carries
//! mutable state (`begin()` → `record_*(&mut self)` → `submit_*(&mut
//! self)`) that doesn't survive duplication.
//!
//! The host `RhiCommandRecorderInner` backing + the methods that name
//! host-only types (`record_buffer_barrier` / `record_copy_*` over
//! `VulkanBufferLike` / `PixelBuffer`, `record_draw*` over
//! `VulkanGraphicsKernel`, `submit_signaling_timeline` over
//! `HostVulkanTimelineSemaphore`, the raw-`vk::*` swapchain / dynamic-
//! rendering methods) stay in the engine — they can't cross the
//! engine-free boundary.

use std::ffi::c_void;

use streamlib_consumer_rhi::VulkanLayout;
use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{GpuContextFullAccessVTable, RhiCommandRecorderMethodsVTable};

use crate::rhi::{Texture, VulkanAccess, VulkanComputeKernel, VulkanStage};

/// Image-to-buffer / buffer-to-image copy region.
///
/// Wraps the most common shape of `VkBufferImageCopy` — single mip
/// level, single array layer, color aspect, full image.
#[derive(Clone, Copy, Debug)]
pub struct ImageCopyRegion {
    pub width: u32,
    pub height: u32,
    pub buffer_offset: u64,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub mip_level: u32,
    pub array_layer: u32,
}

impl ImageCopyRegion {
    /// Tightly-packed region: buffer rows match image width, no offset,
    /// mip 0 / layer 0 / color aspect.
    pub fn tightly_packed(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            buffer_offset: 0,
            buffer_row_length: width,
            buffer_image_height: height,
            mip_level: 0,
            array_layer: 0,
        }
    }
}

/// Engine-owned multi-step command-buffer recorder.
#[repr(C)]
pub struct RhiCommandRecorder {
    /// Opaque handle to the host's `Box<RhiCommandRecorderInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin ABI Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const RhiCommandRecorderMethodsVTable,
}

// SAFETY: handle points at a `Box<RhiCommandRecorderInner>`; the inner is
// Send+Sync (Mutex-guarded state, &mut self method dispatch restricts
// mutation to one thread at a time).
unsafe impl Send for RhiCommandRecorder {}
unsafe impl Sync for RhiCommandRecorder {}

impl RhiCommandRecorder {
    /// Begin a new recording. Dispatches through the per-type methods
    /// vtable's `begin` slot.
    pub fn begin(&mut self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "begin: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; handle paired
        // with it at mint time.
        let status = unsafe {
            ((*self.methods_vtable).begin)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Record an image layout transition. Caller supplies `from_layout`
    /// (typically the texture's last-known layout), the target
    /// `to_layout`, and the surrounding stage/access masks. Dispatches
    /// through the per-type methods vtable's `record_image_barrier`
    /// slot.
    #[allow(clippy::too_many_arguments)]
    pub fn record_image_barrier(
        &mut self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_image_barrier: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; the texture
        // handle is the borrowed `Arc::into_raw(Arc<TextureInner>)`
        // pointer the host reconstructs.
        let status = unsafe {
            ((*self.methods_vtable).record_image_barrier)(
                self.handle,
                texture.handle,
                from_layout.0,
                to_layout.0,
                from_stage.0 as i64,
                to_stage.0 as i64,
                from_access.0 as i64,
                to_access.0 as i64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Record a compute dispatch via the kernel's recorder path.
    /// Bindings + push constants must have been staged on `kernel` via
    /// its `set_*` methods before this call. Dispatches through the
    /// per-type methods vtable's `record_dispatch` slot.
    pub fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "record_dispatch: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; kernel handle is
        // the borrowed `Arc::into_raw(Arc<VulkanComputeKernelInner>)`
        // pointer the host reconstructs.
        let status = unsafe {
            ((*self.methods_vtable).record_dispatch)(
                self.handle,
                kernel.handle,
                group_x,
                group_y,
                group_z,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// End recording and submit without semaphore signaling. The
    /// recorder's internal completion fence is signaled so the next
    /// `begin()` blocks on completion. Dispatches through the per-type
    /// methods vtable's `submit` slot.
    pub fn submit(&mut self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard.
        let status = unsafe {
            ((*self.methods_vtable).submit)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// End recording, submit, and block until the GPU completes.
    /// Dispatches through the per-type methods vtable's `submit_and_wait`
    /// slot.
    pub fn submit_and_wait(&mut self) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "submit_and_wait: command recorder methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard.
        let status = unsafe {
            ((*self.methods_vtable).submit_and_wait)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }
}

/// Decode a `(status, err_buf)` plugin-ABI return into `Result<()>`.
fn status_to_result(status: i32, err_buf: &[u8], err_len: usize) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
        Err(Error::GpuError(msg))
    }
}

impl Drop for RhiCommandRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle is the host's
            // `Box::into_raw(Box<RhiCommandRecorderInner>)`; the vtable's
            // `drop_command_recorder` runs `Box::from_raw + drop`
            // host-side.
            unsafe {
                ((*self.vtable).drop_command_recorder)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for RhiCommandRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandRecorder").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_command_recorder_layout() {
        // Must match the engine's
        // `vulkan/rhi/vulkan_command_recorder.rs::RhiCommandRecorder`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16.
        // Total 24 bytes, align 8.
        assert_eq!(size_of::<RhiCommandRecorder>(), 24);
        assert_eq!(align_of::<RhiCommandRecorder>(), 8);
        assert_eq!(offset_of!(RhiCommandRecorder, handle), 0);
        assert_eq!(offset_of!(RhiCommandRecorder, vtable), 8);
        assert_eq!(offset_of!(RhiCommandRecorder, methods_vtable), 16);
    }

    #[test]
    fn rhi_command_recorder_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiCommandRecorder>();
    }
}
