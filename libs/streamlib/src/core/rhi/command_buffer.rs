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

impl CommandBuffer {
    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &StreamTexture, dst: &StreamTexture) {
        // Metal backend
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            self.inner.copy_texture(&src.inner, &dst.inner);
        }

        // Vulkan backend
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
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
        // Metal backend
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            self.inner.commit();
        }

        // Vulkan backend
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
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
        // Metal backend
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            self.inner.commit_and_wait();
        }

        // Vulkan backend
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            self.inner.commit_and_wait();
        }

        #[cfg(target_os = "windows")]
        {
            self.inner.commit_and_wait();
        }
    }

    /// Get the underlying Metal command buffer (Metal backend only).
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub fn as_metal_command_buffer(&self) -> &crate::metal::rhi::MetalCommandBuffer {
        &self.inner
    }
}

impl std::fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandBuffer").finish()
    }
}
