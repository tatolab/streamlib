// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib surface-adapter smoke fixture for the cuda adapter.
//!
//! Exercises the OPAQUE_FD buffer path the cuda adapter rides for the
//! DLPack zero-copy story:
//!
//!   1. Cdylib's `start()` enters `gpu.escalate(|full| ...)`.
//!   2. `host_vulkan_device_arc()` (v9) → `Arc<HostVulkanDevice>`.
//!   3. Probe `device_arc.opaque_fd_buffer_pool()` — if `None`, the
//!      driver doesn't expose external-memory OPAQUE_FD support; the
//!      fixture writes "SKIP:..." and the test tolerates it as a
//!      clean exit.
//!   4. `HostVulkanBuffer::new_opaque_fd_export(&device_arc, size)` —
//!      OPAQUE_FD-exportable HOST_VISIBLE staging buffer (the shape
//!      CUDA `cudaImportExternalMemory(OPAQUE_FD)` expects).
//!   5. `HostVulkanTimelineSemaphore::new_exportable(device_arc.device(), 0)` —
//!      OPAQUE_FD-exportable timeline (cross-API sync with CUDA).
//!   6. `CudaSurfaceAdapter<HostVulkanDevice>::new(device_arc)` +
//!      `register_host_surface(...)`.
//!   7. `adapter.acquire_write(&surface)` → `view().vk_buffer()`
//!      non-null, drop guard.
//!
//! Output:
//!   - "OK\n<w>x<h>\nvk_buffer=0x<hex>" on full round-trip.
//!   - "SKIP:<reason>" when OPAQUE_FD pool unavailable.
//!   - "ERR:<msg>" on any other step failure.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanTimelineSemaphore};
#[cfg(target_os = "linux")]
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
#[cfg(target_os = "linux")]
use streamlib_adapter_cuda::{
    CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout,
};

#[streamlib::sdk::processor("CudaSmokeTestProcessor")]
pub struct CudaSmokeTest {}

impl ManualProcessor for CudaSmokeTest::Processor {
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
            Ok(SmokeOutcome::Ok(vk_buffer_repr)) => {
                format!("OK\n{W}x{H}\nvk_buffer={vk_buffer_repr}")
            }
            Ok(SmokeOutcome::SkipOpaqueFd(reason)) => format!("SKIP:{reason}"),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "CudaSmokeTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        std::fs::write(&output_path, "SKIP:cuda smoke is linux-only")
            .map_err(|e| {
                Error::Runtime(format!(
                    "CudaSmokeTest: write {output_path}: {e}"
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
    Ok(String),
    SkipOpaqueFd(String),
}

#[cfg(target_os = "linux")]
fn run_smoke(
    ctx: &RuntimeContextFullAccess<'_>,
    surface_id: u64,
    width: u32,
    height: u32,
) -> Result<SmokeOutcome> {
    use streamlib::sdk::engine::host_rhi::HostMarker;

    ctx.gpu_limited_access().escalate(|full| {
        let host_device = full.host_vulkan_device_arc()?;

        // Probe OPAQUE_FD availability before allocating.
        if host_device.opaque_fd_buffer_pool().is_none() {
            return Ok(SmokeOutcome::SkipOpaqueFd(
                "device does not expose OPAQUE_FD HOST_VISIBLE buffer pool".into(),
            ));
        }

        let staging_bytes = (width as u64) * (height as u64) * 4u64;
        let staging = HostVulkanBuffer::new_opaque_fd_export(
            &host_device,
            staging_bytes,
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanBuffer::new_opaque_fd_export: {e}"
            ))
        })?;
        let staging_arc = Arc::new(staging);

        let timeline = HostVulkanTimelineSemaphore::new_exportable(
            host_device.device(),
            0,
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanTimelineSemaphore::new_exportable: {e}"
            ))
        })?;
        let timeline_arc = Arc::new(timeline);

        let adapter = Arc::new(CudaSurfaceAdapter::new(Arc::clone(&host_device)));
        adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration::<HostMarker> {
                    pixel_buffer: Arc::clone(&staging_arc),
                    timeline: Arc::clone(&timeline_arc),
                    initial_layout: VulkanLayout::UNDEFINED,
                },
            )
            .map_err(|e| {
                Error::GpuError(format!(
                    "CudaSurfaceAdapter::register_host_surface: {e:?}"
                ))
            })?;

        let surface = StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        let guard = adapter.acquire_write(&surface).map_err(|e| {
            Error::GpuError(format!(
                "CudaSurfaceAdapter::acquire_write: {e:?}"
            ))
        })?;
        let view = guard.view();
        // `vk::Buffer` displays as `Handle(0x<hex>)`; format via Debug
        // to avoid pulling vulkanalia into the test-fixtures crate.
        let vk_buffer_debug = format!("{:?}", view.vk_buffer());
        if vk_buffer_debug.contains("0x0)") || vk_buffer_debug.ends_with("(0)") {
            return Err(Error::GpuError(format!(
                "vk_buffer() returned a null handle: {vk_buffer_debug}"
            )));
        }
        drop(guard);
        drop(adapter);
        drop(timeline_arc);
        drop(staging_arc);

        Ok(SmokeOutcome::Ok(vk_buffer_debug))
    })
}
