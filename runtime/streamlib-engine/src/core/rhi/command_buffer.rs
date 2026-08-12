// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command buffer abstraction.
//!
//! The handle is `Box::into_raw(Box<CommandBufferInner>)`; `Drop` runs
//! `Box::from_raw + drop`. `commit` / `commit_and_wait` move the inner
//! out by value and null the handle, so the following `Drop` is a no-op.
//!
//! Deliberately NOT `Clone`: command buffers are single-use by
//! contract. Cloning would duplicate the raw `handle` pointer and
//! either double-free the Box on Drop or double-commit on `commit`.

use std::ffi::c_void;

use super::texture::Texture;

/// Rich data backing a [`CommandBuffer`], held behind the opaque handle.
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
/// Command buffers batch GPU operations for submission.
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
pub struct CommandBuffer {
    /// Opaque handle to the host's `Box<CommandBufferInner>`.
    pub(crate) handle: *const c_void,
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
    /// opaque handle.
    pub(crate) fn from_inner(inner: CommandBufferInner) -> Self {
        let handle = Box::into_raw(Box::new(inner)) as *const c_void;
        Self { handle }
    }

    /// Engine-internal mutable borrow of the owned `CommandBufferInner`.
    pub(crate) fn host_inner_mut(&mut self) -> &mut CommandBufferInner {
        // SAFETY: `self.handle` is `Box::into_raw(Box<CommandBufferInner>)`
        // and `&mut self` guarantees no other reference exists.
        unsafe { &mut *(self.handle as *mut CommandBufferInner) }
    }

    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture) {
        if self.handle.is_null() {
            return;
        }
        let inner = self.host_inner_mut();
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            inner
                .inner
                .copy_texture(&src.host_inner().inner, &dst.host_inner().inner);
        }
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            use crate::host_rhi::HostTextureExt;
            inner
                .inner
                .copy_texture(src.vulkan_inner(), dst.vulkan_inner());
        }
    }

    /// Commit the command buffer for execution.
    ///
    /// Consumes the handle: the `Box` is reclaimed and the platform
    /// commit takes the inner by value, so the resources are committed
    /// exactly once. The handle is nulled, making the following `Drop`
    /// a no-op.
    pub fn commit(mut self) {
        tracing::trace!(rhi_op = "queue_submit", "CommandBuffer::commit");
        if !self.handle.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            let inner = unsafe { Box::from_raw(self.handle as *mut CommandBufferInner) };
            self.handle = std::ptr::null();
            inner.inner.commit();
        }
        // `self` drops here; null handle ⇒ Drop no-op.
    }

    /// Commit and wait for completion. Same lifetime contract as
    /// [`Self::commit`].
    pub fn commit_and_wait(mut self) {
        tracing::trace!(
            rhi_op = "queue_submit_and_wait",
            "CommandBuffer::commit_and_wait"
        );
        if !self.handle.is_null() {
            // SAFETY: see `commit` above.
            let inner = unsafe { Box::from_raw(self.handle as *mut CommandBufferInner) };
            self.handle = std::ptr::null();
            inner.inner.commit_and_wait();
        }
    }

    /// Get the underlying Metal command buffer (Metal backend only).
    ///
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub fn as_metal_command_buffer(&self) -> &crate::metal::rhi::MetalCommandBuffer {
        // SAFETY: see `host_inner_mut` — same shape, immutable borrow.
        unsafe { &(*(self.handle as *const CommandBufferInner)).inner }
    }
}

impl Drop for CommandBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            // `commit` / `commit_and_wait` null the handle, so this
            // path is skipped once either has run.
            unsafe {
                drop(Box::from_raw(self.handle as *mut CommandBufferInner));
            }
        }
    }
}

impl std::fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandBuffer").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
