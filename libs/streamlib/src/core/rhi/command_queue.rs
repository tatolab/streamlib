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
#[derive(Clone)]
pub struct RhiCommandQueue {
    #[cfg(target_os = "macos")]
    pub(crate) inner: std::sync::Arc<crate::apple::rhi::MetalCommandQueue>,

    #[cfg(target_os = "linux")]
    pub(crate) inner: std::sync::Arc<crate::linux::rhi::VulkanCommandQueue>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12CommandQueue>,
}

impl RhiCommandQueue {
    /// Create a new command buffer from this queue.
    ///
    /// Command buffers are single-use: create, record commands, commit.
    /// This is the standard pattern for GPU work submission.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        #[cfg(target_os = "macos")]
        {
            let metal_cmd_buffer = self.inner.create_command_buffer()?;
            Ok(CommandBuffer {
                inner: metal_cmd_buffer,
            })
        }

        #[cfg(target_os = "linux")]
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

    /// Get the underlying Metal command queue (macOS only).
    #[cfg(target_os = "macos")]
    pub fn as_metal_command_queue(&self) -> &crate::apple::rhi::MetalCommandQueue {
        &self.inner
    }

    /// Get the raw Metal command queue reference for interop (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_queue_ref(&self) -> &metal::CommandQueueRef {
        self.inner.queue_ref()
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
