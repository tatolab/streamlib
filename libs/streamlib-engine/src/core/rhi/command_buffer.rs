// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command buffer abstraction.
//!
//! Layout-stable `(handle, vtable)` shape. The handle is
//! `Box::into_raw(Box<CommandBufferInner>)`; the vtable's
//! `drop_command_buffer` callback runs `Box::from_raw + drop` on the
//! host side. `commit` / `commit_and_wait` consume the handle
//! similarly (host runs the platform-native commit, then drops the
//! Box; the cdylib's `commit(self)` impl nulls its `handle` field so
//! `Drop` becomes a no-op).
//!
//! Deliberately NOT `Clone`: command buffers are single-use by
//! contract. Cloning would duplicate the raw `handle` pointer and
//! either double-free the Box on Drop or double-commit on `commit`.

use std::ffi::c_void;

use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

use super::texture::Texture;

/// Host-only rich data backing a [`CommandBuffer`]. Cdylib code
/// never sees this type; it reaches the public [`CommandBuffer`]
/// surface through the `(handle, vtable)` β-shape.
///
/// Holds the platform-specific command buffer by value (no Arc —
/// command buffers are single-use, not shared).
pub(crate) struct CommandBufferInner {
    // Metal backend: explicit feature OR macOS/iOS default (when vulkan not requested)
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub(crate) inner: crate::metal::rhi::MetalCommandBuffer,

    // Vulkan backend: explicit feature OR Linux default
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub(crate) inner: crate::vulkan::rhi::VulkanCommandBuffer,

    #[cfg(target_os = "windows")]
    pub(crate) inner: crate::windows::rhi::DX12CommandBuffer,
}

/// Platform-agnostic command buffer wrapper.
///
/// Layout-stable: `#[repr(C)] (handle, vtable)`. Command buffers
/// batch GPU operations for submission.
///
/// On Metal, this wraps MTLCommandBuffer.
/// On Vulkan, this wraps VkCommandBuffer.
/// On DX12, this wraps ID3D12CommandList.
///
/// **Single-use.** Deliberately NOT `Clone` — `commit(self)` and
/// `commit_and_wait(self)` consume the handle by value. The
/// `compile_fail` doctest below locks the no-Clone contract.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib::sdk::rhi::CommandBuffer>();
/// ```
#[repr(C)]
pub struct CommandBuffer {
    /// Opaque handle to the host's `Box<CommandBufferInner>`.
    pub(crate) handle: *const c_void,
    /// Vtable for cross-DSO Drop and method dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at a `Box<CommandBufferInner>` that owns its
// underlying platform-native command buffer. Send/Sync follow from the
// platform-native command buffer's own contract (VkCommandBuffer is
// Send; MTLCommandBuffer is Send via Apple's threading guarantees;
// DX12 command lists are likewise).
unsafe impl Send for CommandBuffer {}
unsafe impl Sync for CommandBuffer {}

impl CommandBuffer {
    /// Internal helper: leak a `Box<CommandBufferInner>` as the
    /// opaque handle and resolve the host-mode vtable.
    pub(crate) fn from_inner(inner: CommandBufferInner) -> Self {
        let handle = Box::into_raw(Box::new(inner)) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self { handle, vtable }
    }

    /// Engine-internal mutable borrow of the host-owned
    /// `CommandBufferInner`. **Panics if called from cdylib code.**
    pub(crate) fn host_inner_mut(&mut self) -> &mut CommandBufferInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "CommandBuffer::host_inner_mut() reached from cdylib code; this method \
                 must dispatch through the GpuContextLimitedAccessVTable."
            );
        }
        // SAFETY: `self.handle` is `Box::into_raw(Box<CommandBufferInner>)`
        // and `&mut self` guarantees no other reference exists.
        unsafe { &mut *(self.handle as *mut CommandBufferInner) }
    }

    /// Copy one texture to another.
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `copy_texture_command_buffer` callback.
    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction. We pass
        // typed `*const Texture` pointers; the layout is locked by
        // per-type `texture_layout` regression test so the host's read agrees
        // with the cdylib's write.
        unsafe {
            ((*self.vtable).copy_texture_command_buffer)(
                self.handle,
                src as *const Texture as *const c_void,
                dst as *const Texture as *const c_void,
            );
        }
    }

    /// Commit the command buffer for execution.
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `commit_command_buffer` callback. The host's impl runs
    /// `Box::from_raw + commit + drop` so the underlying platform
    /// resources are committed exactly once. The cdylib's local
    /// `handle` / `vtable` fields are nulled so `Drop` becomes a
    /// no-op when `self` falls out of scope.
    pub fn commit(mut self) {
        tracing::trace!(rhi_op = "queue_submit", "CommandBuffer::commit");
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle + vtable were paired at construction;
            // commit takes ownership of the Box host-side. After this
            // call the cdylib's handle is stale.
            unsafe {
                ((*self.vtable).commit_command_buffer)(self.handle);
            }
            // Null fields so Drop is a no-op (host already dropped the
            // Box during commit).
            self.handle = std::ptr::null();
            self.vtable = std::ptr::null();
        }
        // `self` drops here; null handle ⇒ Drop no-op.
    }

    /// Commit and wait for completion.
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `commit_and_wait_command_buffer` callback. Same lifetime
    /// contract as [`Self::commit`].
    pub fn commit_and_wait(mut self) {
        tracing::trace!(
            rhi_op = "queue_submit_and_wait",
            "CommandBuffer::commit_and_wait"
        );
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: see `commit` above.
            unsafe {
                ((*self.vtable).commit_and_wait_command_buffer)(self.handle);
            }
            self.handle = std::ptr::null();
            self.vtable = std::ptr::null();
        }
    }

    /// Get the underlying Metal command buffer (Metal backend only).
    ///
    /// Engine-internal — panics from cdylib code via `host_inner_mut`.
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub fn as_metal_command_buffer(&self) -> &crate::metal::rhi::MetalCommandBuffer {
        // SAFETY: see `host_inner_mut` — same shape, immutable borrow.
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "CommandBuffer::as_metal_command_buffer() reached from cdylib code"
            );
        }
        unsafe { &(*(self.handle as *const CommandBufferInner)).inner }
    }
}

impl Drop for CommandBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            // After `commit` / `commit_and_wait` ran, fields are
            // null so this path is skipped (the Box was already
            // freed host-side during commit).
            unsafe {
                ((*self.vtable).drop_command_buffer)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandBuffer").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn command_buffer_layout() {
        // Pin the byte-level shape. Fields:
        //   handle : *const c_void → offset 0, size 8
        //   vtable : *const VTable → offset 8, size 8
        // Total: 16 bytes, 8-byte alignment.
        assert_eq!(size_of::<CommandBuffer>(), 16);
        assert_eq!(align_of::<CommandBuffer>(), 8);
        assert_eq!(offset_of!(CommandBuffer, handle), 0);
        assert_eq!(offset_of!(CommandBuffer, vtable), 8);
    }

    #[test]
    fn command_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CommandBuffer>();
    }

    /// `CommandBuffer` is intentionally NOT `Clone`: commit-semantics
    /// consume the handle. The contract is locked by the
    /// `compile_fail` doctest on the type — this `#[test]` is a
    /// discoverability marker so the witness shows up in
    /// `cargo test` output.
    #[test]
    fn command_buffer_is_not_clone_doc_witness() {
        // No-op — see the type-level compile_fail doctest.
    }
}
