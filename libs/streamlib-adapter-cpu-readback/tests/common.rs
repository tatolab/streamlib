// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the cpu-readback adapter integration
//! tests. Pulled in via `#[path = "common.rs"] mod common;` from each
//! test file.
//!
//! Post-#562 (Path E single-pattern shape) every host-side test
//! pre-allocates the staging buffers + timeline directly via the RHI
//! and constructs the adapter with an in-process trigger. No
//! surface-share IPC, no CpuReadbackBridge wiring — the host adapter
//! sees the imported staging buffers natively as
//! `Arc<HostVulkanPixelBuffer>` through `HostSurfaceRegistration<HostMarker>`.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // each test file uses a different subset

use std::sync::{Arc, OnceLock};

use streamlib::host_rhi::{
    HostMarker, HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore,
};
use streamlib::core::context::GpuContext;
use streamlib::core::error::StreamError;
use streamlib::core::rhi::{PixelFormat, TextureFormat};
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_cpu_readback::{
    CpuReadbackContext, CpuReadbackCopyTrigger, CpuReadbackSurfaceAdapter,
    HostSurfaceRegistration, InProcessCpuReadbackCopyTrigger, VulkanLayout,
};

/// Convenience alias — every host-side test instantiates the adapter
/// against a real `HostVulkanDevice`.
pub type HostAdapter = CpuReadbackSurfaceAdapter<HostVulkanDevice>;
pub type HostContext = CpuReadbackContext<HostVulkanDevice>;

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
    pub adapter: Arc<HostAdapter>,
    pub ctx: HostContext,
}

impl HostFixture {
    pub fn try_new() -> Option<Self> {
        let gpu = try_init_gpu()?;
        let host_device = Arc::clone(gpu.device().vulkan_device());
        let trigger = Arc::new(InProcessCpuReadbackCopyTrigger::new(Arc::clone(
            &host_device,
        ))) as Arc<dyn CpuReadbackCopyTrigger<HostMarker>>;
        let adapter = Arc::new(CpuReadbackSurfaceAdapter::new(
            Arc::clone(&host_device),
            trigger,
        ));
        let ctx = CpuReadbackContext::new(Arc::clone(&adapter));
        Some(Self { gpu, adapter, ctx })
    }

    /// Allocate a host BGRA8 `VkImage` + per-plane staging buffer +
    /// exportable timeline, register them with the adapter under
    /// `surface_id`, and return a [`StreamlibSurface`] descriptor
    /// pointing at the registration.
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

    /// Fallible variant — returns `Err(StreamError)` when the host
    /// can't allocate a render-target DMA-BUF in `texture_format` on
    /// this driver (typically NV12 RT modifier missing on the EGL
    /// probe). Multi-plane tests use this to skip cleanly.
    pub fn try_register_surface_with_format(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        surface_format: SurfaceFormat,
        texture_format: TextureFormat,
    ) -> Result<StreamlibSurface, StreamError> {
        let stream_texture =
            self.gpu
                .acquire_render_target_dma_buf_image(width, height, texture_format)?;
        let texture_arc = Arc::clone(stream_texture.vulkan_inner());

        // Allocate one HOST_VISIBLE staging buffer per logical plane.
        let plane_count = surface_format.plane_count();
        let mut staging_planes: Vec<Arc<HostVulkanPixelBuffer>> =
            Vec::with_capacity(plane_count as usize);
        for plane_idx in 0..plane_count {
            let plane_w = surface_format.plane_width(width, plane_idx);
            let plane_h = surface_format.plane_height(height, plane_idx);
            let plane_bpp = surface_format.plane_bytes_per_pixel(plane_idx);
            let pf = staging_pixel_format_for(surface_format, plane_idx);
            let pb = HostVulkanPixelBuffer::new(
                self.adapter.device(),
                plane_w,
                plane_h,
                plane_bpp,
                pf,
            )
            .map_err(|e| {
                StreamError::GpuError(format!("staging plane {plane_idx}: {e}"))
            })?;
            staging_planes.push(Arc::new(pb));
        }

        let timeline = Arc::new(
            HostVulkanTimelineSemaphore::new(self.adapter.device().device(), 0)
                .map_err(|e| StreamError::GpuError(format!("create timeline: {e}")))?,
        );
        self.adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration::<HostMarker> {
                    texture: Some(texture_arc),
                    staging_planes,
                    timeline,
                    initial_image_layout: VulkanLayout::UNDEFINED,
                    format: surface_format,
                    width,
                    height,
                },
            )
            .map_err(|e| {
                StreamError::GpuError(format!("register_host_surface: {e:?}"))
            })?;
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

/// Pixel format used to label staging-buffer allocations. The adapter
/// drives copy geometry from `SurfaceFormat`'s plane functions, so
/// the staging `PixelFormat` is bookkeeping only — pick something
/// that matches the byte layout for clarity.
fn staging_pixel_format_for(format: SurfaceFormat, plane: u32) -> PixelFormat {
    match (format, plane) {
        (SurfaceFormat::Bgra8, 0) => PixelFormat::Bgra32,
        (SurfaceFormat::Rgba8, 0) => PixelFormat::Rgba32,
        // NV12 plane 0 (Y) is one-byte-per-texel; plane 1 (UV) is two.
        // Neither maps cleanly to a CV pixel format constant, so use
        // `Gray8` which the adapter ignores.
        (SurfaceFormat::Nv12, _) => PixelFormat::Gray8,
        _ => PixelFormat::Unknown,
    }
}
