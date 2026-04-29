// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::conformance` — runs the
//! public `run_conformance` suite from `streamlib-adapter-abi` against
//! a real cpu-readback adapter wired to a host-allocated DMA-BUF
//! `VkImage`, per-plane staging buffers, and an exportable timeline
//! semaphore.
//!
//! Same eight contracts the Vulkan / OpenGL adapters pass: acquire/drop
//! pairs, parallel reads, `WriteContended` on contention,
//! `try_acquire_*` returning `Ok(None)` on contention, and Send+Sync
//! under multi-thread reads.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;

use streamlib::host_rhi::{HostMarker, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
use streamlib::core::rhi::{PixelFormat, TextureFormat};
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId,
};
use streamlib_adapter_cpu_readback::{HostSurfaceRegistration, VulkanLayout};

use common::HostFixture;

struct ConformanceFactory<'a> {
    fixture: &'a HostFixture,
}

impl<'a> streamlib_adapter_abi::testing::ConformanceSurfaceFactory
    for ConformanceFactory<'a>
{
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        self.fixture.register_surface(id, 64, 64)
    }
}

#[test]
fn cpu_readback_adapter_passes_run_conformance() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "cpu-readback conformance: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let factory = ConformanceFactory { fixture: &fixture };
    run_conformance(&*fixture.adapter, factory);

    let bogus = empty_surface(0xdead_beef);
    match fixture.adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}

#[test]
fn duplicate_registration_returns_surface_already_registered() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "cpu-readback duplicate-registration: skipping — no Vulkan device available"
            );
            return;
        }
    };
    let id: SurfaceId = 0xfeed_face;
    let _first = fixture.register_surface(id, 64, 64);

    // Build a fresh registration for the same id and assert the
    // adapter rejects it as `SurfaceAlreadyRegistered` rather than
    // the overloaded `SurfaceNotFound`.
    let stream_texture = fixture
        .gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let texture_arc = Arc::clone(stream_texture.vulkan_inner());
    let staging = Arc::new(
        HostVulkanPixelBuffer::new(fixture.adapter.device(), 64, 64, 4, PixelFormat::Bgra32)
            .expect("staging plane"),
    );
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(fixture.adapter.device().device(), 0)
            .expect("timeline"),
    );
    let result = fixture.adapter.register_host_surface(
        id,
        HostSurfaceRegistration::<HostMarker> {
            texture: Some(texture_arc),
            staging_planes: vec![staging],
            timeline,
            initial_image_layout: VulkanLayout::UNDEFINED,
            format: SurfaceFormat::Bgra8,
            width: 64,
            height: 64,
        },
    );
    match result {
        Err(AdapterError::SurfaceAlreadyRegistered { surface_id }) => {
            assert_eq!(surface_id, id);
        }
        other => panic!("expected SurfaceAlreadyRegistered, got {other:?}"),
    }
}
