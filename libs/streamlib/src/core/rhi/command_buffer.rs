// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command buffer abstraction.

use super::texture::StreamTexture;

/// Platform-agnostic command buffer wrapper.
///
/// Command buffers batch GPU operations for submission.
/// On Metal, this wraps MTLCommandBuffer.
/// On Vulkan, this wraps VkCommandBuffer.
/// On DX12, this wraps ID3D12CommandList.
pub struct CommandBuffer {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::apple::rhi::MetalCommandBuffer,

    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::linux::rhi::VulkanCommandBuffer,

    #[cfg(target_os = "windows")]
    pub(crate) inner: crate::windows::rhi::DX12CommandBuffer,
}

impl CommandBuffer {
    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &StreamTexture, dst: &StreamTexture) {
        #[cfg(target_os = "macos")]
        {
            self.inner.copy_texture(&src.inner, &dst.inner);
        }

        #[cfg(target_os = "linux")]
        {
            self.inner.copy_texture(&src.inner, &dst.inner);
        }

        #[cfg(target_os = "windows")]
        {
            self.inner.copy_texture(&src.inner, &dst.inner);
        }
    }

    /// Commit the command buffer for execution.
    pub fn commit(self) {
        #[cfg(target_os = "macos")]
        {
            self.inner.commit();
        }

        #[cfg(target_os = "linux")]
        {
            self.inner.commit();
        }

        #[cfg(target_os = "windows")]
        {
            self.inner.commit();
        }
    }

    /// Commit and wait for completion.
    pub fn commit_and_wait(self) {
        #[cfg(target_os = "macos")]
        {
            self.inner.commit_and_wait();
        }

        #[cfg(target_os = "linux")]
        {
            self.inner.commit_and_wait();
        }

        #[cfg(target_os = "windows")]
        {
            self.inner.commit_and_wait();
        }
    }

    /// Get the underlying Metal command buffer (macOS only).
    #[cfg(target_os = "macos")]
    pub fn as_metal_command_buffer(&self) -> &crate::apple::rhi::MetalCommandBuffer {
        &self.inner
    }
}

impl std::fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandBuffer").finish()
    }
}
