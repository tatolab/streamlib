// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_vulkan::tests::conformance` — runs the public
//! `run_conformance` suite from `streamlib-adapter-abi` against a real
//! Vulkan adapter wired to a host-allocated DMA-BUF render-target image
//! and an exportable timeline semaphore.
//!
//! Exercises the same eight contracts MockAdapter passes (acquire/drop
//! pairs, parallel reads, `WriteContended` on contention, `try_acquire_*`
//! returning `Ok(None)`, multi-thread Send+Sync). A green run confirms
//! the trait shape is honored — it does NOT prove cross-process
//! correctness; that's the round-trip and crash tests.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_vulkan::{HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter};

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_vulkan=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

fn register_one(
    adapter: &VulkanSurfaceAdapter<HostVulkanDevice>,
    gpu: &GpuContext,
    id: SurfaceId,
) -> StreamlibSurface {
    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0).expect("timeline"),
    );
    adapter
        .register_host_surface(
            id,
            HostSurfaceRegistration {
                texture,
                timeline,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register_host_surface");
    StreamlibSurface::new(
        id,
        64,
        64,
        SurfaceFormat::Bgra8,
        SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    )
}

struct ConformanceFactory<'a> {
    adapter: &'a VulkanSurfaceAdapter<HostVulkanDevice>,
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
fn vulkan_adapter_passes_run_conformance() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("vulkan-adapter conformance: skipping — no Vulkan device available");
            return;
        }
    };
    let adapter = VulkanSurfaceAdapter::new(Arc::clone(gpu.device().vulkan_device()));

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

#[test]
fn duplicate_registration_returns_surface_already_registered() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("vulkan-adapter duplicate-registration: skipping — no Vulkan device available");
            return;
        }
    };
    let adapter = VulkanSurfaceAdapter::new(Arc::clone(gpu.device().vulkan_device()));
    let id: SurfaceId = 0xfeed_face;
    let _first = register_one(&adapter, &gpu, id);

    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0).expect("timeline"),
    );
    let result = adapter.register_host_surface(
        id,
        HostSurfaceRegistration {
            texture,
            timeline,
            initial_layout: VulkanLayout::UNDEFINED,
        },
    );
    match result {
        Err(AdapterError::SurfaceAlreadyRegistered { surface_id }) => {
            assert_eq!(surface_id, id);
        }
        other => panic!("expected SurfaceAlreadyRegistered, got {other:?}"),
    }
}
