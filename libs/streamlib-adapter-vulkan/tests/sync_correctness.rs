// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_vulkan::tests::sync_correctness` — confirms that
//! the adapter advances the appropriate timeline semaphore on guard
//! drop and that downstream observers see the post-release value.
//!
//! Single-writer-per-edge per
//! `docs/architecture/adapter-timeline-single-writer.md`: dropping a
//! write guard signals `produce_done`; dropping the last read guard
//! signals `consume_done`. Each timeline carries the shared
//! `current_signal_value` counter, so signals across the two timelines
//! advance the underlying counter in interleaving order.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use streamlib::sdk::engine::{HostGpuDeviceExt, HostTextureExt};

use streamlib::sdk::engine::host_rhi::HostVulkanTimelineSemaphore;
use streamlib::sdk::context::GpuContext;
use streamlib::sdk::rhi::TextureFormat;
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
    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let produce_done = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0)
            .expect("create produce_done"),
    );
    let consume_done = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0)
            .expect("create consume_done"),
    );
    adapter
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                texture,
                produce_done: Arc::clone(&produce_done),
                consume_done: Arc::clone(&consume_done),
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

    // Initial counters are 0 and acquire/wait succeeds.
    assert_eq!(produce_done.current_value().unwrap(), 0);
    assert_eq!(consume_done.current_value().unwrap(), 0);

    {
        let _w = ctx.acquire_write(&descriptor).expect("acquire_write 1");
        assert_eq!(
            produce_done.current_value().unwrap(),
            0,
            "no produce_done signal while write guard is alive"
        );
    }
    // Write drop signals produce_done with the shared
    // current_signal_value counter (now 1). consume_done is untouched.
    assert_eq!(
        produce_done.current_value().unwrap(),
        1,
        "write drop signals produce_done"
    );
    assert_eq!(
        consume_done.current_value().unwrap(),
        0,
        "write drop does not touch consume_done"
    );

    {
        let _r = ctx.acquire_read(&descriptor).expect("acquire_read after w1");
    }
    // Read drop signals consume_done with current_signal_value=2;
    // produce_done is untouched.
    assert_eq!(
        consume_done.current_value().unwrap(),
        2,
        "read drop signals consume_done"
    );
    assert_eq!(
        produce_done.current_value().unwrap(),
        1,
        "read drop does not touch produce_done"
    );

    // Two parallel readers share a release boundary — consume_done
    // advances exactly once on the LAST reader's drop.
    let r1 = ctx.acquire_read(&descriptor).expect("acquire_read r1");
    let r2 = ctx.acquire_read(&descriptor).expect("acquire_read r2");
    assert_eq!(
        consume_done.current_value().unwrap(),
        2,
        "no consume_done signal mid-concurrent-read"
    );
    drop(r1);
    assert_eq!(
        consume_done.current_value().unwrap(),
        2,
        "first reader drop is silent"
    );
    drop(r2);
    assert_eq!(
        consume_done.current_value().unwrap(),
        3,
        "last reader drop signals consume_done once"
    );
    assert_eq!(
        produce_done.current_value().unwrap(),
        1,
        "no read-path signal ever touched produce_done"
    );
}
