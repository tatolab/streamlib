// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the cpu-readback adapter integration
//! tests. Pulled in via `#[path = "common.rs"] mod common;` from each
//! test file.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // each test file uses a different subset

use std::sync::{Arc, OnceLock};

use streamlib::adapter_support::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::error::StreamError;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_cpu_readback::{
    CpuReadbackContext, CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
};
use vulkanalia::vk;

/// Shared `GpuContext` for every test in this binary. Two `VkDevice`s
/// in one process race on NVIDIA Linux (see
/// `docs/learnings/nvidia-dual-vulkan-device-crash.md`); a single
/// shared device is the standard mitigation for parallel-test
/// execution.
static SHARED_GPU: OnceLock<Option<Arc<GpuContext>>> = OnceLock::new();

pub fn try_init_gpu() -> Option<Arc<GpuContext>> {
    SHARED_GPU
        .get_or_init(|| {
            let _ = tracing_subscriber::fmt()
                .with_test_writer()
                .with_env_filter("streamlib_adapter_cpu_readback=debug,streamlib=warn")
                .try_init();
            GpuContext::init_for_platform_sync().ok().map(Arc::new)
        })
        .clone()
}

pub struct HostFixture {
    pub gpu: Arc<GpuContext>,
    pub adapter: Arc<CpuReadbackSurfaceAdapter>,
    pub ctx: CpuReadbackContext,
}

impl HostFixture {
    pub fn try_new() -> Option<Self> {
        let gpu = try_init_gpu()?;
        let adapter = Arc::new(CpuReadbackSurfaceAdapter::new(Arc::clone(
            gpu.device().vulkan_device(),
        )));
        let ctx = CpuReadbackContext::new(Arc::clone(&adapter));
        Some(Self { gpu, adapter, ctx })
    }

    /// Allocate a host BGRA8 `VkImage` + exportable timeline, register
    /// them with the adapter under `surface_id`, and return a
    /// [`StreamlibSurface`] descriptor pointing at the registration.
    pub fn register_surface(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
    ) -> StreamlibSurface {
        self.register_surface_with_format(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            TextureFormat::Bgra8Unorm,
        )
    }

    /// General-purpose surface registration used by single- and multi-
    /// plane tests. `surface_format` is the customer-facing pixel
    /// format; `texture_format` is the RHI-level texture allocation
    /// format. They must agree (e.g. `Nv12` ↔ `TextureFormat::Nv12`).
    pub fn register_surface_with_format(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        surface_format: SurfaceFormat,
        texture_format: TextureFormat,
    ) -> StreamlibSurface {
        self.try_register_surface_with_format(
            surface_id,
            width,
            height,
            surface_format,
            texture_format,
        )
        .expect("register_surface_with_format")
    }

    /// Fallible variant. Returns `Err(StreamError)` when the host can't
    /// allocate a render-target DMA-BUF in `texture_format` on this
    /// driver — typically because the EGL probe didn't advertise an
    /// RT-capable DRM modifier for the format. Multi-plane tests use
    /// this so they can skip cleanly on drivers without NV12 RT modifier
    /// support, instead of failing.
    pub fn try_register_surface_with_format(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        surface_format: SurfaceFormat,
        texture_format: TextureFormat,
    ) -> Result<StreamlibSurface, StreamError> {
        let texture = self
            .gpu
            .acquire_render_target_dma_buf_image(width, height, texture_format)?;
        let timeline = Arc::new(
            HostVulkanTimelineSemaphore::new(self.adapter.device().device(), 0)
                .map_err(|e| StreamError::GpuError(format!("create timeline: {e}")))?,
        );
        self.adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration {
                    texture,
                    timeline,
                    initial_image_layout: vk::ImageLayout::UNDEFINED.as_raw(),
                    format: surface_format,
                },
            )
            .map_err(|e| StreamError::GpuError(format!("register_host_surface: {e}")))?;
        Ok(StreamlibSurface::new(
            surface_id,
            width,
            height,
            surface_format,
            SurfaceUsage::CPU_READBACK,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        ))
    }
}
