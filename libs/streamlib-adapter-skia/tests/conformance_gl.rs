// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_skia::tests::conformance_gl` — runs the public
//! `run_conformance` suite from `streamlib-adapter-abi` against the
//! GL-backed Skia adapter wired to a host-allocated DMA-BUF render-
//! target image and a real surfaceless EGL+GL context.
//!
//! Mirror of `conformance.rs` (Vulkan-backed) but composed on
//! `OpenGlSurfaceAdapter`. A green run confirms the trait-composition
//! shape from #509 / #511 holds for GL too — `for<'g>
//! Inner::WriteView<'g>: GlWritable` is satisfied by the inner
//! OpenGl adapter's views, and Skia's `Surface` / `Image` propagate
//! the `Send + Sync` invariant the conformance suite's parallel-
//! readers test demands.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter, DRM_FORMAT_ARGB8888,
};
use streamlib_adapter_skia::SkiaGlSurfaceAdapter;

fn try_init() -> Option<(GpuContext, Arc<EglRuntime>)> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            "streamlib_adapter_skia=debug,streamlib_adapter_opengl=warn,streamlib=warn",
        )
        .try_init();
    let gpu = GpuContext::init_for_platform_sync().ok()?;
    let egl = match EglRuntime::new() {
        Ok(r) => r,
        Err(e) => {
            println!("conformance_gl: skipping — EGL unavailable: {e}");
            return None;
        }
    };
    Some((gpu, egl))
}

fn register_one(
    inner: &OpenGlSurfaceAdapter,
    gpu: &GpuContext,
    id: SurfaceId,
) -> StreamlibSurface {
    let texture = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let dma_buf_fd = texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let plane_layout = texture
        .vulkan_inner()
        .dma_buf_plane_layout()
        .expect("dma_buf_plane_layout");
    let modifier = texture.vulkan_inner().chosen_drm_format_modifier();

    let registration = HostSurfaceRegistration {
        dma_buf_fd,
        width: 64,
        height: 64,
        drm_fourcc: DRM_FORMAT_ARGB8888,
        drm_format_modifier: modifier,
        plane_offset: plane_layout[0].0,
        plane_stride: plane_layout[0].1,
    };
    inner
        .register_host_surface(id, registration)
        .expect("register_host_surface");
    // We deliberately leak the `StreamTexture` here for the lifetime
    // of the test — the GL adapter holds the DMA-BUF FD imported via
    // EGL, and the host VkImage backing must outlive every guard the
    // conformance suite acquires.
    std::mem::forget(texture);
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
    inner: &'a OpenGlSurfaceAdapter,
    gpu: &'a GpuContext,
}

impl streamlib_adapter_abi::testing::ConformanceSurfaceFactory for ConformanceFactory<'_> {
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        register_one(self.inner, self.gpu, id)
    }
}

#[test]
fn skia_gl_adapter_passes_run_conformance() {
    let (gpu, egl) = match try_init() {
        Some(t) => t,
        None => {
            println!("skia-gl-adapter conformance: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let inner = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
    let skia_gl_adapter = match SkiaGlSurfaceAdapter::new(Arc::clone(&inner)) {
        Ok(a) => a,
        Err(e) => {
            println!("skia-gl-adapter conformance: skipping — Skia DirectContext build failed: {e}");
            return;
        }
    };

    let factory = ConformanceFactory {
        inner: inner.as_ref(),
        gpu: &gpu,
    };
    run_conformance(&skia_gl_adapter, factory);

    // Unknown surface id must propagate as SurfaceNotFound through
    // the composed adapter. The Skia GL adapter delegates registration
    // to the inner OpenGl adapter, so the inner adapter is the source
    // of the error — we just verify it travels back unchanged.
    let bogus = empty_surface(0xdead_beef);
    match skia_gl_adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
