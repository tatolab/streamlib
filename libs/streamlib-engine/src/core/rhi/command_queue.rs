// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command queue abstraction.
//!
//! Layout-stable `(handle, vtable)` shape. The handle is
//! `Arc::into_raw(Arc<RhiCommandQueueInner>)`; the vtable's
//! `clone_rhi_command_queue` / `drop_rhi_command_queue` callbacks
//! manage the Arc refcount in host-compiled code.
//!
//! Platform-specific Arcs (`VulkanCommandQueue` on Linux,
//! `MetalCommandQueue` on macOS) live on the private
//! [`RhiCommandQueueInner`] type behind the opaque handle.

use std::ffi::c_void;

use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

use crate::core::Result;

use super::CommandBuffer;

/// Host-only rich data backing a [`RhiCommandQueue`]. Cdylib code
/// never sees this type; it reaches the public [`RhiCommandQueue`]
/// surface through the `(handle, vtable)` PluginAbiObject.
pub(crate) struct RhiCommandQueueInner {
    // Metal backend: explicit feature OR macOS/iOS default (when vulkan not requested)
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub(crate) inner: std::sync::Arc<crate::metal::rhi::MetalCommandQueue>,

    // Vulkan backend: explicit feature OR Linux default
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::VulkanCommandQueue>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12CommandQueue>,

    /// Metal command queue for Apple platform services.
    /// Always present on macOS/iOS regardless of GPU backend selection.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) metal_queue: std::sync::Arc<crate::metal::rhi::MetalCommandQueue>,
}

/// Platform-agnostic command queue wrapper.
///
/// Layout-stable: `#[repr(C)] (handle, vtable)`. The command queue is
/// created once per device and shared across all processors. Use
/// [`create_command_buffer`](RhiCommandQueue::create_command_buffer)
/// to create single-use command buffers for GPU operations.
///
/// On Metal, this wraps MTLCommandQueue.
/// On Vulkan, this wraps VkQueue.
/// On DX12, this wraps ID3D12CommandQueue.
///
/// On macOS/iOS, Metal queue is always available for Apple platform services
/// regardless of which GPU backend is selected for rendering.
#[repr(C)]
pub struct RhiCommandQueue {
    /// Opaque handle to the host's `Arc<RhiCommandQueueInner>`.
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop and method dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at an `Arc<RhiCommandQueueInner>` whose
// interior is Send+Sync (command queues are thread-safe by design).
// Refcount management crosses the cdylib boundary through the vtable
// but runs in host-compiled code regardless.
unsafe impl Send for RhiCommandQueue {}
unsafe impl Sync for RhiCommandQueue {}

impl RhiCommandQueue {
    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, resolve the host-mode vtable, and assemble
    /// the plugin ABI shape.
    pub(crate) fn from_arc_into_raw(arc: std::sync::Arc<RhiCommandQueueInner>) -> Self {
        let handle = std::sync::Arc::into_raw(arc) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self { handle, vtable }
    }

    /// Engine-internal borrow of the host-owned `RhiCommandQueueInner`.
    /// **Panics if called from cdylib code.**
    pub(crate) fn host_inner(&self) -> &RhiCommandQueueInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "RhiCommandQueue::host_inner() reached from cdylib code; this method \
                 must dispatch through the GpuContextLimitedAccessVTable."
            );
        }
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`.
        // The leaked strong count keeps the inner alive at least until Drop.
        unsafe { &*(self.handle as *const RhiCommandQueueInner) }
    }

    /// Create a new command buffer from this queue.
    ///
    /// Command buffers are single-use: create, record commands, commit.
    /// This is the standard pattern for GPU work submission.
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `create_command_buffer_from_queue` callback.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(crate::core::Error::GpuError(
                "create_command_buffer: RhiCommandQueue has null handle/vtable".into(),
            ));
        }
        let mut out_cb: std::mem::MaybeUninit<CommandBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable were paired at construction.
        let status = unsafe {
            ((*self.vtable).create_command_buffer_from_queue)(
                self.handle,
                out_cb.as_mut_ptr() as *mut c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid CommandBuffer.
            Ok(unsafe { out_cb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(crate::core::Error::GpuError(msg))
        }
    }

    /// Get the underlying Metal command queue for Apple platform services.
    ///
    /// Available on macOS/iOS regardless of which GPU backend is selected.
    /// Engine-internal — panics from cdylib code via `host_inner`.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_metal_command_queue(&self) -> &crate::metal::rhi::MetalCommandQueue {
        &self.host_inner().metal_queue
    }

    /// Get the raw Metal command queue reference for Apple platform services.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn metal_queue_ref(&self) -> &metal::CommandQueueRef {
        self.host_inner().metal_queue.queue_ref()
    }
}

impl Clone for RhiCommandQueue {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction; the
            // vtable's `clone_rhi_command_queue` contract is
            // `Arc::increment_strong_count(handle)` on the host side.
            unsafe {
                ((*self.vtable).clone_rhi_command_queue)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for RhiCommandQueue {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `clone_rhi_command_queue` bumps.
            unsafe {
                ((*self.vtable).drop_rhi_command_queue)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for RhiCommandQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandQueue").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_command_queue_layout() {
        // Pin the byte-level shape. Fields:
        //   handle : *const c_void → offset 0, size 8
        //   vtable : *const VTable → offset 8, size 8
        // Total: 16 bytes, 8-byte alignment.
        assert_eq!(size_of::<RhiCommandQueue>(), 16);
        assert_eq!(align_of::<RhiCommandQueue>(), 8);
        assert_eq!(offset_of!(RhiCommandQueue, handle), 0);
        assert_eq!(offset_of!(RhiCommandQueue, vtable), 8);
    }

    #[test]
    fn rhi_command_queue_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiCommandQueue>();
    }
}
