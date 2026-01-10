// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI command queue abstraction.

use crate::core::Result;

use super::CommandBuffer;

/// Platform-agnostic command queue wrapper.
///
/// The command queue is created once per device and shared across all processors.
/// Use [`create_command_buffer`](RhiCommandQueue::create_command_buffer) to create
/// single-use command buffers for GPU operations.
///
/// On Metal, this wraps MTLCommandQueue.
/// On Vulkan, this wraps VkQueue.
/// On DX12, this wraps ID3D12CommandQueue.
///
/// On macOS/iOS, Metal queue is always available for Apple platform services
/// regardless of which GPU backend is selected for rendering.
#[derive(Clone)]
pub struct RhiCommandQueue {
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

impl RhiCommandQueue {
    /// Create a new command buffer from this queue.
    ///
    /// Command buffers are single-use: create, record commands, commit.
    /// This is the standard pattern for GPU work submission.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        // Metal backend
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            let metal_cmd_buffer = self.inner.create_command_buffer()?;
            Ok(CommandBuffer {
                inner: metal_cmd_buffer,
            })
        }

        // Vulkan backend
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            let vulkan_cmd_buffer = self.inner.create_command_buffer()?;
            Ok(CommandBuffer {
                inner: vulkan_cmd_buffer,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_cmd_buffer = self.inner.create_command_buffer()?;
            Ok(CommandBuffer {
                inner: dx12_cmd_buffer,
            })
        }
    }

    /// Get the underlying Metal command queue for Apple platform services.
    ///
    /// Available on macOS/iOS regardless of which GPU backend is selected.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_metal_command_queue(&self) -> &crate::metal::rhi::MetalCommandQueue {
        &self.metal_queue
    }

    /// Get the raw Metal command queue reference for Apple platform services.
    ///
    /// Available on macOS/iOS regardless of which GPU backend is selected.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn metal_queue_ref(&self) -> &metal::CommandQueueRef {
        self.metal_queue.queue_ref()
    }
}

impl std::fmt::Debug for RhiCommandQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandQueue").finish()
    }
}

// RhiCommandQueue is Send + Sync because command queues are thread-safe
unsafe impl Send for RhiCommandQueue {}
unsafe impl Sync for RhiCommandQueue {}
