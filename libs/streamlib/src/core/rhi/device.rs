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
///
/// On macOS/iOS, Metal is always available for Apple platform services (IOSurface,
/// CVPixelBuffer, etc.) regardless of which GPU backend is selected for rendering.
#[derive(Clone)]
pub struct GpuDevice {
    // Metal backend: when vulkan NOT requested AND (explicit metal feature OR macOS/iOS)
    // Vulkan takes precedence if explicitly requested
    #[cfg(all(
        not(feature = "backend-vulkan"),
        any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
    ))]
    pub(crate) inner: std::sync::Arc<crate::metal::rhi::MetalDevice>,

    // Vulkan backend: explicit feature OR Linux default (when metal not requested)
    #[cfg(any(
        feature = "backend-vulkan",
        all(target_os = "linux", not(feature = "backend-metal"))
    ))]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::VulkanDevice>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12Device>,

    /// Metal device for Apple platform services.
    /// Always present on macOS/iOS regardless of GPU backend selection.
    /// Used for IOSurface, CVPixelBuffer, and other Apple-specific operations.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) metal_device: std::sync::Arc<crate::metal::rhi::MetalDevice>,

    /// Shared command queue for all GPU operations.
    command_queue: RhiCommandQueue,
}

impl GpuDevice {
    /// Create a new GPU device using the system default.
    pub fn new() -> Result<Self> {
        // Metal backend (default on macOS/iOS when Vulkan not requested)
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            let metal_device = crate::metal::rhi::MetalDevice::new()?;
            let metal_queue_wrapper = metal_device.create_command_queue_wrapper();
            let metal_queue_arc = std::sync::Arc::new(metal_queue_wrapper);
            let command_queue = RhiCommandQueue {
                inner: metal_queue_arc.clone(),
                metal_queue: metal_queue_arc,
            };
            let metal_device_arc = std::sync::Arc::new(metal_device);
            Ok(Self {
                inner: metal_device_arc.clone(),
                metal_device: metal_device_arc,
                command_queue,
            })
        }

        // Vulkan backend (explicit feature OR Linux default)
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            let vulkan_device = crate::vulkan::rhi::VulkanDevice::new()?;
            let vulkan_queue = vulkan_device.create_command_queue_wrapper();

            // On macOS/iOS with Vulkan backend, also create Metal device/queue for Apple services
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            let (metal_device, metal_queue) = {
                let md = crate::metal::rhi::MetalDevice::new()?;
                let mq = md.create_command_queue_wrapper();
                (std::sync::Arc::new(md), std::sync::Arc::new(mq))
            };

            let command_queue = RhiCommandQueue {
                inner: std::sync::Arc::new(vulkan_queue),
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                metal_queue,
            };

            Ok(Self {
                inner: std::sync::Arc::new(vulkan_device),
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                metal_device,
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
        // Metal backend
        #[cfg(all(
            not(feature = "backend-vulkan"),
            any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
        ))]
        {
            let metal_texture = self.inner.create_texture(desc)?;
            Ok(StreamTexture::from_metal(metal_texture))
        }

        // Vulkan backend
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
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

    /// Get the underlying Metal device for Apple platform services.
    ///
    /// Available on macOS/iOS regardless of which GPU backend is selected.
    /// Apple services (IOSurface, CVPixelBuffer, VideoToolbox) require Metal.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn as_metal_device(&self) -> &crate::metal::rhi::MetalDevice {
        &self.metal_device
    }

    /// Get the raw Metal device handle for Apple platform services.
    ///
    /// Available on macOS/iOS regardless of which GPU backend is selected.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn metal_device_ref(&self) -> &metal::DeviceRef {
        self.metal_device.device_ref()
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
