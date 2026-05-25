// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib surface-adapter smoke fixture for the opengl adapter.
//!
//! Exercises the full cdylib-side adapter-construction chain end-to-end:
//!
//!   1. Cdylib's `start()` enters `gpu.escalate(|full| ...)`.
//!   2. `host_vulkan_device_arc()` (v9) → `Arc<HostVulkanDevice>`.
//!   3. `full.acquire_render_target_dma_buf_image()` → `Texture`
//!      β-shape; `host_vulkan_texture_arc()` (v10) →
//!      `Arc<HostVulkanTexture>`.
//!   4. Pub methods on the texture Arc — `export_dma_buf_fd()`,
//!      `dma_buf_plane_layout()`, `chosen_drm_format_modifier()` —
//!      collect the fields the opengl `HostSurfaceRegistration` needs.
//!   5. `EglRuntime::new()` — may fail when EGL display / extensions
//!      aren't available; in that case the smoke writes "SKIP:..." and
//!      the test tolerates it as a clean exit.
//!   6. `OpenGlSurfaceAdapter::new(runtime)` +
//!      `register_host_surface(...)`.
//!   7. `adapter.acquire_write(&surface)` → `gl_texture_id()` non-zero,
//!      drop guard.
//!
//! Output:
//!   - "OK\n<w>x<h>\ngl_texture=<n>" on full round-trip.
//!   - "SKIP:<reason>" when EGL init fails (host lacks EGL display);
//!     the integration test treats this as a clean pass.
//!   - "ERR:<msg>" on any other step failure.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::HostTextureExt;
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::TextureFormat;
#[cfg(target_os = "linux")]
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
#[cfg(target_os = "linux")]
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter, DRM_FORMAT_ARGB8888,
};

#[streamlib::sdk::processor("OpenGlSmokeTestProcessor")]
pub struct OpenGlSmokeTest {}

impl ManualProcessor for OpenGlSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        const W: u32 = 64;
        const H: u32 = 64;
        const SURFACE_ID: u64 = 1;

        let line = match run_smoke(ctx, SURFACE_ID, W, H) {
            Ok(SmokeOutcome::Ok(gl_texture)) => {
                format!("OK\n{W}x{H}\ngl_texture={gl_texture}")
            }
            Ok(SmokeOutcome::SkipEgl(reason)) => format!("SKIP:{reason}"),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "OpenGlSmokeTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        std::fs::write(&output_path, "SKIP:opengl smoke is linux-only")
            .map_err(|e| {
                Error::Runtime(format!(
                    "OpenGlSmokeTest: write {output_path}: {e}"
                ))
            })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
enum SmokeOutcome {
    Ok(u32),
    SkipEgl(String),
}

#[cfg(target_os = "linux")]
fn run_smoke(
    ctx: &RuntimeContextFullAccess<'_>,
    surface_id: u64,
    width: u32,
    height: u32,
) -> Result<SmokeOutcome> {
    ctx.gpu_limited_access().escalate(|full| {
        let _host_device = full.host_vulkan_device_arc()?;
        let stream_texture = full.acquire_render_target_dma_buf_image(
            width,
            height,
            TextureFormat::Bgra8Unorm,
        )?;
        let texture_arc = stream_texture.host_vulkan_texture_arc()?;

        let dma_buf_fd = texture_arc.export_dma_buf_fd().map_err(|e| {
            Error::GpuError(format!("HostVulkanTexture::export_dma_buf_fd: {e}"))
        })?;
        let plane_layout = texture_arc.dma_buf_plane_layout().map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanTexture::dma_buf_plane_layout: {e}"
            ))
        })?;
        let modifier = texture_arc.chosen_drm_format_modifier();

        // EGL init may fail when the test host lacks a display, lacks
        // the required EGL extensions, or runs in a strict sandbox.
        // Treat that as a clean SKIP — the cdylib-reachability story
        // up to this point has already been validated.
        let egl = match EglRuntime::new() {
            Ok(r) => r,
            Err(e) => {
                return Ok(SmokeOutcome::SkipEgl(format!("EglRuntime::new: {e:?}")));
            }
        };

        let adapter = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
        adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration {
                    dma_buf_fd,
                    width,
                    height,
                    drm_fourcc: DRM_FORMAT_ARGB8888,
                    drm_format_modifier: modifier,
                    plane_offset: plane_layout[0].0,
                    plane_stride: plane_layout[0].1,
                },
            )
            .map_err(|e| {
                Error::GpuError(format!(
                    "OpenGlSurfaceAdapter::register_host_surface: {e:?}"
                ))
            })?;

        let surface = StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        let guard = adapter.acquire_write(&surface).map_err(|e| {
            Error::GpuError(format!(
                "OpenGlSurfaceAdapter::acquire_write: {e:?}"
            ))
        })?;
        let view = guard.view();
        let gl_texture = view.gl_texture_id();
        if gl_texture == 0 {
            return Err(Error::GpuError(
                "gl_texture_id() returned zero".into(),
            ));
        }
        drop(guard);
        drop(adapter);
        drop(texture_arc);

        Ok(SmokeOutcome::Ok(gl_texture))
    })
}
