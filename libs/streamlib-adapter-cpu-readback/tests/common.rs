// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the cpu-readback adapter integration
//! tests. Pulled in via `#[path = "common.rs"] mod common;` from each
//! test file.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // each test file uses a different subset

use std::sync::{Arc, OnceLock};

use streamlib::adapter_support::VulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
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

    /// Allocate a host `VkImage` + exportable timeline, register them
    /// with the adapter under `surface_id`, and return a
    /// [`StreamlibSurface`] descriptor pointing at the registration.
    pub fn register_surface(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
    ) -> StreamlibSurface {
        let texture = self
            .gpu
            .acquire_render_target_dma_buf_image(width, height, TextureFormat::Bgra8Unorm)
            .expect("acquire_render_target_dma_buf_image");
        let timeline = Arc::new(
            VulkanTimelineSemaphore::new(self.adapter.device().device(), 0)
                .expect("create timeline"),
        );
        self.adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration {
                    texture,
                    timeline,
                    initial_image_layout: vk::ImageLayout::UNDEFINED.as_raw(),
                    bytes_per_pixel: 4,
                },
            )
            .expect("register_host_surface");
        StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::CPU_READBACK,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        )
    }
}
