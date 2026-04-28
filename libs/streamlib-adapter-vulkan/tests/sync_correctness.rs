// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_vulkan::tests::sync_correctness` — confirms that
//! the adapter advances the timeline semaphore on guard drop and that
//! a downstream consumer of the same timeline observes the post-release
//! value.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::adapter_support::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_vulkan::{
    HostSurfaceRegistration, VulkanContext, VulkanLayout, VulkanSurfaceAdapter,
};

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_vulkan=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

#[test]
fn timeline_counter_advances_on_release_and_is_observable_by_next_acquire() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("sync_correctness: skipping — no Vulkan device available");
            return;
        }
    };

    let adapter = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(
        gpu.device().vulkan_device(),
    )));
    let ctx = VulkanContext::new(Arc::clone(&adapter));

    let surface_id: SurfaceId = 1;
    let texture = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0)
            .expect("create timeline"),
    );
    adapter
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                texture,
                timeline: Arc::clone(&timeline),
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register host surface");

    let descriptor = StreamlibSurface::new(
        surface_id,
        64,
        64,
        SurfaceFormat::Bgra8,
        SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );

    // Initial counter is 0 and acquire/wait succeeds.
    assert_eq!(timeline.current_value().unwrap(), 0);

    {
        let _w = ctx.acquire_write(&descriptor).expect("acquire_write 1");
        assert_eq!(
            timeline.current_value().unwrap(),
            0,
            "no signal while guard is alive"
        );
    }
    assert_eq!(timeline.current_value().unwrap(), 1, "drop signals once");

    {
        let _r = ctx.acquire_read(&descriptor).expect("acquire_read after w1");
    }
    assert_eq!(timeline.current_value().unwrap(), 2);

    // Two parallel readers share a release boundary — the counter
    // advances exactly once on the LAST reader's drop.
    let r1 = ctx.acquire_read(&descriptor).expect("acquire_read r1");
    let r2 = ctx.acquire_read(&descriptor).expect("acquire_read r2");
    assert_eq!(
        timeline.current_value().unwrap(),
        2,
        "no signal mid-concurrent-read"
    );
    drop(r1);
    assert_eq!(
        timeline.current_value().unwrap(),
        2,
        "first reader drop is silent"
    );
    drop(r2);
    assert_eq!(
        timeline.current_value().unwrap(),
        3,
        "last reader drop signals once"
    );
}
