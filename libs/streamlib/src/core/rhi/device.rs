// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI device abstraction.

use crate::core::Result;

use super::texture::{StreamTexture, TextureDescriptor};

/// Platform-agnostic GPU device wrapper.
///
/// This type wraps the platform-specific device implementation and provides
/// a unified interface for GPU operations. Use the `as_*` methods to "dip down"
/// to the native device when needed for platform-specific operations.
#[derive(Clone)]
pub struct GpuDevice {
    #[cfg(target_os = "macos")]
    pub(crate) inner: std::sync::Arc<crate::apple::rhi::MetalDevice>,

    #[cfg(target_os = "linux")]
    pub(crate) inner: std::sync::Arc<crate::linux::rhi::VulkanDevice>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12Device>,
}

impl GpuDevice {
    /// Create a new GPU device using the system default.
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let metal_device = crate::apple::rhi::MetalDevice::new()?;
            Ok(Self {
                inner: std::sync::Arc::new(metal_device),
            })
        }

        #[cfg(target_os = "linux")]
        {
            let vulkan_device = crate::linux::rhi::VulkanDevice::new()?;
            Ok(Self {
                inner: std::sync::Arc::new(vulkan_device),
            })
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_device = crate::windows::rhi::DX12Device::new()?;
            Ok(Self {
                inner: std::sync::Arc::new(dx12_device),
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
