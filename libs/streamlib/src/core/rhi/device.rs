// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI device abstraction.

use crate::core::Result;

use super::command_queue::RhiCommandQueue;
use super::texture::{StreamTexture, TextureDescriptor};

/// Platform-agnostic GPU device wrapper.
///
/// This type wraps the platform-specific device implementation and provides
/// a unified interface for GPU operations. Use the `as_*` methods to "dip down"
/// to the native device when needed for platform-specific operations.
///
/// Includes a shared command queue created at device initialization.
/// All processors should use this shared queue via [`command_queue`](GpuDevice::command_queue).
#[derive(Clone)]
pub struct GpuDevice {
    #[cfg(target_os = "macos")]
    pub(crate) inner: std::sync::Arc<crate::apple::rhi::MetalDevice>,

    #[cfg(target_os = "linux")]
    pub(crate) inner: std::sync::Arc<crate::linux::rhi::VulkanDevice>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12Device>,

    /// Shared command queue for all GPU operations.
    command_queue: RhiCommandQueue,
}

impl GpuDevice {
    /// Create a new GPU device using the system default.
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let metal_device = crate::apple::rhi::MetalDevice::new()?;
            let metal_queue = metal_device.create_command_queue_wrapper();
            let command_queue = RhiCommandQueue {
                inner: std::sync::Arc::new(metal_queue),
            };
            Ok(Self {
                inner: std::sync::Arc::new(metal_device),
                command_queue,
            })
        }

        #[cfg(target_os = "linux")]
        {
            let vulkan_device = crate::linux::rhi::VulkanDevice::new()?;
            let vulkan_queue = vulkan_device.create_command_queue_wrapper();
            let command_queue = RhiCommandQueue {
                inner: std::sync::Arc::new(vulkan_queue),
            };
            Ok(Self {
                inner: std::sync::Arc::new(vulkan_device),
                command_queue,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_device = crate::windows::rhi::DX12Device::new()?;
            let dx12_queue = dx12_device.create_command_queue_wrapper();
            let command_queue = RhiCommandQueue {
                inner: std::sync::Arc::new(dx12_queue),
            };
            Ok(Self {
                inner: std::sync::Arc::new(dx12_device),
                command_queue,
            })
        }
    }

    /// Create a texture on this device.
    pub fn create_texture(&self, desc: &TextureDescriptor) -> Result<StreamTexture> {
        #[cfg(target_os = "macos")]
        {
            let metal_texture = self.inner.create_texture(desc)?;
            Ok(StreamTexture::from_metal(metal_texture))
        }

        #[cfg(target_os = "linux")]
        {
            let vulkan_texture = self.inner.create_texture(desc)?;
            Ok(StreamTexture::from_vulkan(vulkan_texture))
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_texture = self.inner.create_texture(desc)?;
            Ok(StreamTexture::from_dx12(dx12_texture))
        }
    }

    /// Get the shared command queue.
    ///
    /// All processors should use this shared queue rather than creating their own.
    /// The queue is created once at device initialization and reused.
    pub fn command_queue(&self) -> &RhiCommandQueue {
        &self.command_queue
    }

    /// Get the underlying Metal device (macOS only).
    #[cfg(target_os = "macos")]
    pub fn as_metal_device(&self) -> &crate::apple::rhi::MetalDevice {
        &self.inner
    }

    /// Get the raw Metal device handle (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_device_ref(&self) -> &metal::DeviceRef {
        self.inner.device_ref()
    }
}

impl std::fmt::Debug for GpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDevice").finish()
    }
}

// GpuDevice is Send + Sync because the inner device types are
unsafe impl Send for GpuDevice {}
unsafe impl Sync for GpuDevice {}
