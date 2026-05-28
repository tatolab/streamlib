// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib surface-adapter smoke fixture for the vulkan adapter.
//!
//! Exercises the full cdylib-side adapter-construction chain end-to-end:
//!
//!   1. Cdylib's `start()` enters `gpu.escalate(|full| ...)`.
//!   2. `host_vulkan_device_arc()` (v9) → `Arc<HostVulkanDevice>`.
//!   3. `full.acquire_render_target_dma_buf_image(...)` → `Texture`
//!      β-shape; `host_vulkan_texture_arc()` (v10) →
//!      `Arc<HostVulkanTexture>`.
//!   4. `HostVulkanTimelineSemaphore::new(device_arc.device(), 0)` —
//!      cdylib-reachable direct constructor (non-exportable variant
//!      is fine for in-process adapter use).
//!   5. `VulkanSurfaceAdapter<HostVulkanDevice>::new(device_arc)` +
//!      `register_host_surface(...)`.
//!   6. `acquire_write(&surface)` → `vk_image()`, assert the handle is
//!      non-null, drop the guard.
//!
//! Output:
//!   - "OK\n<width>x<height>\nvk_image=0x<hex>" on full round-trip.
//!   - "ERR:<message>" on any step failure.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::host_rhi::HostVulkanTimelineSemaphore;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::HostTextureExt;
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::TextureFormat;
#[cfg(target_os = "linux")]
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage, VulkanWritable,
};
#[cfg(target_os = "linux")]
use streamlib_adapter_vulkan::{
    HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter,
};

#[streamlib::sdk::processor("VulkanSmokeTestProcessor")]
pub struct VulkanSmokeTest {}

impl ManualProcessor for VulkanSmokeTest::Processor {
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
            Ok(vk_image_raw) => {
                format!("OK\n{W}x{H}\nvk_image=0x{vk_image_raw:x}")
            }
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "VulkanSmokeTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        std::fs::write(&output_path, "ERR:vulkan smoke is linux-only")
            .map_err(|e| {
                Error::Runtime(format!(
                    "VulkanSmokeTest: write {output_path}: {e}"
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
fn run_smoke(
    ctx: &RuntimeContextFullAccess<'_>,
    surface_id: u64,
    width: u32,
    height: u32,
) -> Result<u64> {
    // Manual-mode start() takes FullAccess directly; the engine
    // wraps cdylib lifecycle dispatch in `with_cdylib_scope` (#1075),
    // so `ctx.gpu_full_access()` is `ScopeToken`-flavored and
    // dispatches through the FullAccess vtable transparently.
    // Same coverage as the pre-#1075 escalate path; the wrap is the
    // engine-side replacement for the explicit `.escalate(|full|...)`.
    let full = ctx.gpu_full_access();

    let host_device = full.host_vulkan_device_arc()?;
    let stream_texture = full.acquire_render_target_dma_buf_image(
        width,
        height,
        TextureFormat::Bgra8Unorm,
    )?;
    let texture_arc = stream_texture.host_vulkan_texture_arc()?;

    // Single-writer-per-edge per
    // `docs/architecture/adapter-timeline-single-writer.md`: two
    // independent timelines, one per direction.
    let produce_done = Arc::new(
        HostVulkanTimelineSemaphore::new(host_device.device(), 0).map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanTimelineSemaphore::new (produce_done): {e}"
            ))
        })?,
    );
    let consume_done = Arc::new(
        HostVulkanTimelineSemaphore::new(host_device.device(), 0).map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanTimelineSemaphore::new (consume_done): {e}"
            ))
        })?,
    );

    let adapter = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(&host_device)));
    adapter
        .register_host_surface(
            surface_id,
            HostSurfaceRegistration {
                texture: Arc::clone(&texture_arc),
                produce_done: Arc::clone(&produce_done),
                consume_done: Arc::clone(&consume_done),
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "VulkanSurfaceAdapter::register_host_surface: {e:?}"
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
            "VulkanSurfaceAdapter::acquire_write: {e:?}"
        ))
    })?;
    let view = guard.view();
    let vk_image_raw = view.vk_image().0;
    if vk_image_raw == 0 {
        return Err(Error::GpuError(
            "vk_image() returned a null handle".into(),
        ));
    }
    drop(guard);
    drop(adapter);
    drop(produce_done);
    drop(consume_done);
    drop(texture_arc);

    Ok(vk_image_raw)
}
