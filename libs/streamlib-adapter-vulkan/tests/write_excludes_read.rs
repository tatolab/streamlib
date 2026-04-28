// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_vulkan::tests::write_excludes_read` — explicit
//! redundant-with-conformance test asserting that an active write guard
//! makes a concurrent `acquire_read` fail with `WriteContended`, and that
//! the read succeeds once the write guard drops.
//!
//! Conformance covers the same contract; this exists as a focused
//! regression that surfaces clearly in CI logs when contention semantics
//! drift.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::adapter_support::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
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
fn live_write_guard_blocks_acquire_read_until_dropped() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("write_excludes_read: skipping — no Vulkan device available");
            return;
        }
    };

    let adapter = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(
        gpu.device().vulkan_device(),
    )));
    let ctx = VulkanContext::new(Arc::clone(&adapter));

    let surface_id: SurfaceId = 7;
    let stream_tex = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture = stream_tex.vulkan_inner().clone();
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0).expect("timeline"),
    );
    adapter
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                texture,
                timeline,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register");

    let descriptor = StreamlibSurface::new(
        surface_id,
        64,
        64,
        SurfaceFormat::Bgra8,
        SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );

    let writer = ctx.acquire_write(&descriptor).expect("first acquire_write");
    match ctx.acquire_read(&descriptor) {
        Err(AdapterError::WriteContended { surface_id: id, .. }) => {
            assert_eq!(id, surface_id);
        }
        Err(other) => panic!("expected WriteContended, got {other:?}"),
        Ok(_) => panic!("acquire_read must fail while write held"),
    }
    drop(writer);

    let _r = ctx
        .acquire_read(&descriptor)
        .expect("acquire_read after writer dropped");
}
