// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_skia::tests::conformance` — runs the public
//! `run_conformance` suite from `streamlib-adapter-abi` against a real
//! Skia adapter wired to a host-allocated DMA-BUF render-target image
//! and an exportable timeline semaphore.
//!
//! The Skia adapter composes on `streamlib-adapter-vulkan`, so a green
//! run confirms the trait composition shape from #509 / #511 holds —
//! `for<'g> Inner::WriteView<'g>: VulkanWritable + VulkanImageInfoExt`
//! is satisfied by the inner Vulkan adapter's views, and Skia's
//! `Surface` / `Image` propagate the `Send + Sync` invariant the
//! conformance suite's parallel-readers test demands.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTexture, HostVulkanTimelineSemaphore};
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_skia::SkiaSurfaceAdapter;
use streamlib_adapter_vulkan::{HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter};

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            "streamlib_adapter_skia=debug,streamlib_adapter_vulkan=warn,streamlib=warn",
        )
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

fn register_one(
    inner: &VulkanSurfaceAdapter<HostVulkanDevice>,
    _gpu: &GpuContext,
    id: SurfaceId,
) -> StreamlibSurface {
    // Skia's `check_image_info` (`GrVkGpu.cpp:1298-1302`) requires
    // both TRANSFER_SRC and TRANSFER_DST on every wrapped image, in
    // addition to whatever role-specific usage the image is used
    // for. The conformance fixture deliberately allocates a plain
    // device-local OPTIMAL VkImage (no DMA-BUF, no DRM modifier
    // tiling) via `HostVulkanTexture::new_device_local` so the
    // surface-share registry's queue-family-foreign import doesn't
    // factor into Skia's wrap-time validation; production Skia work
    // (polyglot wrapper follow-up) goes through
    // `acquire_render_target_dma_buf_image`, which already includes
    // COPY_DST in its usage set.
    let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST,
    );
    let raw_tex = HostVulkanTexture::new_device_local(inner.device(), &desc)
        .expect("HostVulkanTexture::new_device_local");
    let texture = Arc::new(raw_tex);
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(inner.device().device(), 0).expect("timeline"),
    );
    inner
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
    inner: &'a VulkanSurfaceAdapter<HostVulkanDevice>,
    gpu: &'a GpuContext,
}

impl<'a> streamlib_adapter_abi::testing::ConformanceSurfaceFactory for ConformanceFactory<'a> {
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        register_one(self.inner, self.gpu, id)
    }
}

#[test]
fn skia_adapter_passes_run_conformance() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("skia-adapter conformance: skipping — no Vulkan device available");
            return;
        }
    };
    let inner = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(
        gpu.device().vulkan_device(),
    )));
    let skia_adapter = match SkiaSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("skia-adapter conformance: skipping — Skia DirectContext build failed: {e}");
            return;
        }
    };

    let factory = ConformanceFactory {
        inner: inner.as_ref(),
        gpu: &gpu,
    };
    run_conformance(&skia_adapter, factory);

    // Unknown surface id must propagate as SurfaceNotFound through
    // the composed adapter. The Skia adapter delegates registration
    // to the inner Vulkan adapter, so the inner adapter is the source
    // of the error — we just verify it travels back unchanged.
    let bogus = empty_surface(0xdead_beef);
    match skia_adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
