// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command queue abstraction.
//!
//! Layout-stable `(handle, vtable)` shape. The handle is
//! `Arc::into_raw(Arc<RhiCommandQueueInner>)`; the vtable's
//! `clone_rhi_command_queue` / `drop_rhi_command_queue` callbacks
//! manage the Arc refcount in host-compiled code.
//!
//! The platform-specific Arc lives on the private
//! [`RhiCommandQueueInner`] type behind the opaque handle.

use std::ffi::c_void;
use std::sync::Arc;

use crate::core::Result;

use super::CommandBuffer;

/// Rich data backing a [`RhiCommandQueue`], reached through the
/// queue's opaque handle.
pub(crate) struct RhiCommandQueueInner {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::VulkanCommandQueue>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12CommandQueue>,
}

/// Platform-agnostic command queue wrapper.
///
/// Layout-stable: `#[repr(C)] (handle, vtable)`. The command queue is
/// created once per device and shared across all processors. Use
/// [`create_command_buffer`](RhiCommandQueue::create_command_buffer)
/// to create single-use command buffers for GPU operations.
///
/// On Vulkan, this wraps VkQueue.
/// On DX12, this wraps ID3D12CommandQueue.
#[repr(C)]
pub struct RhiCommandQueue {
    /// Opaque handle to the host's `Arc<RhiCommandQueueInner>`.
    pub(crate) handle: *const c_void,
}

// SAFETY: `handle` points at an `Arc<RhiCommandQueueInner>` whose
// interior is Send+Sync (command queues are thread-safe by design).
// Refcount management crosses the cdylib boundary through the vtable
// but runs in host-compiled code regardless.
unsafe impl Send for RhiCommandQueue {}
unsafe impl Sync for RhiCommandQueue {}

impl RhiCommandQueue {
    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw` and wrap it as the opaque handle.
    pub(crate) fn from_arc_into_raw(arc: Arc<RhiCommandQueueInner>) -> Self {
        let handle = Arc::into_raw(arc) as *const c_void;
        Self { handle }
    }

    /// Engine-internal borrow of the host-owned `RhiCommandQueueInner`.
    pub(crate) fn host_inner(&self) -> &RhiCommandQueueInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`.
        // The leaked strong count keeps the inner alive at least until Drop.
        unsafe { &*(self.handle as *const RhiCommandQueueInner) }
    }

    /// Create a new command buffer from this queue.
    ///
    /// Command buffers are single-use: create, record commands, commit.
    /// This is the standard pattern for GPU work submission.
    ///
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        if self.handle.is_null() {
            return Err(crate::core::Error::GpuError(
                "create_command_buffer: RhiCommandQueue has null handle".into(),
            ));
        }
        let platform_command_buffer = self.host_inner().inner.create_command_buffer()?;
        Ok(CommandBuffer::from_inner(
            crate::core::rhi::command_buffer::CommandBufferInner {
                inner: platform_command_buffer,
            },
        ))
    }
}

impl Clone for RhiCommandQueue {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const RhiCommandQueueInner);
            }
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for RhiCommandQueue {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const RhiCommandQueueInner);
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

    #[test]
    fn rhi_command_queue_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiCommandQueue>();
    }
}
