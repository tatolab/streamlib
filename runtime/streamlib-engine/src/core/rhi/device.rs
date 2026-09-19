// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI device abstraction.

use crate::core::Result;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::host_rhi::HostTextureExt;

use super::command_queue::RhiCommandQueue;
use super::texture::{Texture, TextureDescriptor};

/// Platform-agnostic GPU device wrapper.
///
/// This type wraps the platform-specific device implementation and provides
/// a unified interface for GPU operations. Engine code reaches the native
/// device through the [`crate::host_rhi::HostGpuDeviceExt`] extension trait.
///
/// Includes a shared command queue created at device initialization.
/// All processors should use this shared queue via [`command_queue`](GpuDevice::command_queue).
#[derive(Clone)]
pub struct GpuDevice {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) inner: std::sync::Arc<crate::vulkan::rhi::HostVulkanDevice>,

    #[cfg(target_os = "windows")]
    pub(crate) inner: std::sync::Arc<crate::windows::rhi::DX12Device>,

    /// Shared command queue for all GPU operations.
    command_queue: RhiCommandQueue,
}

impl GpuDevice {
    // Privileged Host-flavor accessor (`vulkan_device`) lives on the
    // [`crate::host_rhi::HostGpuDeviceExt`] extension trait —
    // type-system-enforced boundary so the SDK's public inherent impl
    // stays Host-free. Engine RHI helpers and in-tree adapters
    // `use crate::host_rhi::HostGpuDeviceExt;` to surface it.

    /// Create a new GPU device using the system default.
    pub fn new() -> Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let device_arc = crate::vulkan::rhi::HostVulkanDevice::new()?;
            let vulkan_queue = device_arc.create_command_queue_wrapper();

            let command_queue = {
                let inner = crate::core::rhi::command_queue::RhiCommandQueueInner {
                    inner: std::sync::Arc::new(vulkan_queue),
                };
                RhiCommandQueue::from_arc_into_raw(std::sync::Arc::new(inner))
            };

            // Store a global reference for DMA-BUF import (Linux only).
            // The import trait (RhiPixelBufferImport::from_external_handle) is a
            // static method with no device parameter, so the global bridges that gap.
            #[cfg(target_os = "linux")]
            {
                if crate::vulkan::rhi::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
                    .set(std::sync::Arc::clone(&device_arc))
                    .is_err()
                {
                    tracing::warn!(
                        "VULKAN_DEVICE_FOR_IMPORT already set (duplicate GpuDevice::new() call)"
                    );
                }
            }

            Ok(Self {
                inner: device_arc,
                command_queue,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_device = crate::windows::rhi::DX12Device::new()?;
            let dx12_queue = dx12_device.create_command_queue_wrapper();
            let command_queue = {
                let inner = crate::core::rhi::command_queue::RhiCommandQueueInner {
                    inner: std::sync::Arc::new(dx12_queue),
                };
                RhiCommandQueue::from_arc_into_raw(std::sync::Arc::new(inner))
            };
            Ok(Self {
                inner: std::sync::Arc::new(dx12_device),
                command_queue,
            })
        }
    }

    /// Create a texture on this device.
    pub fn create_texture(&self, desc: &TextureDescriptor) -> Result<Texture> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let vulkan_texture = self.inner.create_texture(desc)?;
            Ok(Texture::from_vulkan(vulkan_texture))
        }

        #[cfg(target_os = "windows")]
        {
            let dx12_texture = self.inner.create_texture(desc)?;
            Ok(Texture::from_dx12(dx12_texture))
        }
    }

    /// Create a non-exportable, device-local texture for same-process consumers.
    ///
    /// Skips the DMA-BUF export pool where applicable. Use for textures that
    /// never cross process boundaries; reduces pressure on NVIDIA Linux's
    /// DMA-BUF allocation cap.
    #[cfg(target_os = "linux")]
    pub fn create_texture_local(&self, desc: &TextureDescriptor) -> Result<Texture> {
        let vulkan_texture = self.inner.create_texture_local(desc)?;
        Ok(Texture::from_vulkan(vulkan_texture))
    }

    /// Create an OPAQUE_FD-exportable texture a foreign process can import
    /// whole — see [`crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export`]
    /// for the fixed usage set and the CUDA-mappable format subset.
    #[cfg(target_os = "linux")]
    pub fn create_texture_opaque_fd_export(&self, desc: &TextureDescriptor) -> Result<Texture> {
        let vulkan_texture = self.inner.create_texture_opaque_fd_export(desc)?;
        Ok(Texture::from_vulkan(vulkan_texture))
    }

    /// Create an explicit-DRM-modifier DMA-BUF texture a foreign process can
    /// import as the same tiled image. Errors when the format has no DRM
    /// FOURCC or the EGL probe advertised no RT-capable modifier for it.
    #[cfg(target_os = "linux")]
    pub fn create_texture_render_target_dma_buf(
        &self,
        desc: &TextureDescriptor,
    ) -> Result<Texture> {
        let vulkan_texture = self.inner.create_texture_render_target_dma_buf(desc)?;
        Ok(Texture::from_vulkan(vulkan_texture))
    }

    /// Get the shared command queue.
    ///
    /// All processors should use this shared queue rather than creating their own.
    /// The queue is created once at device initialization and reused.
    pub fn command_queue(&self) -> &RhiCommandQueue {
        &self.command_queue
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
