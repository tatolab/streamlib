// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::conformance` — runs the
//! public `run_conformance` suite from `streamlib-adapter-abi` against
//! a real cpu-readback adapter wired to a host-allocated DMA-BUF
//! `VkImage` and an exportable timeline semaphore.
//!
//! Same eight contracts the Vulkan / OpenGL adapters pass: acquire/drop
//! pairs, parallel reads, `WriteContended` on contention,
//! `try_acquire_*` returning `Ok(None)` on contention, and Send+Sync
//! under multi-thread reads.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::host_rhi::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cpu_readback::{
    CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
};
use vulkanalia::vk;

fn try_init_gpu() -> Option<Arc<GpuContext>> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cpu_readback=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok().map(Arc::new)
}

fn register_one(
    adapter: &CpuReadbackSurfaceAdapter,
    gpu: &GpuContext,
    id: SurfaceId,
) -> StreamlibSurface {
    let texture = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0)
            .expect("create timeline"),
    );
    adapter
        .register_host_surface(
            id,
            HostSurfaceRegistration {
                texture,
                timeline,
                initial_image_layout: vk::ImageLayout::UNDEFINED.as_raw(),
                format: SurfaceFormat::Bgra8,
            },
        )
        .expect("register_host_surface");
    StreamlibSurface::new(
        id,
        64,
        64,
        SurfaceFormat::Bgra8,
        // CPU_READBACK is the canonical usage for surfaces backed by
        // this adapter — surfaces that ride the cpu-readback exit are
        // marked at allocation time, separate from RENDER_TARGET /
        // SAMPLED users.
        SurfaceUsage::CPU_READBACK,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    )
}

struct ConformanceFactory<'a> {
    adapter: &'a CpuReadbackSurfaceAdapter,
    gpu: &'a GpuContext,
}

impl<'a> streamlib_adapter_abi::testing::ConformanceSurfaceFactory
    for ConformanceFactory<'a>
{
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        register_one(self.adapter, self.gpu, id)
    }
}

#[test]
fn cpu_readback_adapter_passes_run_conformance() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!(
                "cpu-readback conformance: skipping — no Vulkan device available"
            );
            return;
        }
    };
    let adapter =
        CpuReadbackSurfaceAdapter::new(Arc::clone(gpu.device().vulkan_device()));

    let factory = ConformanceFactory {
        adapter: &adapter,
        gpu: &gpu,
    };
    run_conformance(&adapter, factory);

    let bogus = empty_surface(0xdead_beef);
    match adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
