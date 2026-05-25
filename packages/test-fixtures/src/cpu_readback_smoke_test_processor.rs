// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib surface-adapter smoke fixture for the cpu-readback
//! adapter family.
//!
//! Exercises the full cdylib-side adapter-construction chain end-to-end:
//!
//!   1. Cdylib's `start()` enters `gpu.escalate(|full| ...)`.
//!   2. Inside the closure: obtain `Arc<HostVulkanDevice>` via the v9
//!      `host_vulkan_device_arc` FullAccess vtable bridge.
//!   3. Allocate a HOST_VISIBLE staging `HostVulkanBuffer` via the
//!      `Arc<HostVulkanDevice>` — verifies the cdylib-reachable
//!      constructor path documented on `HostVulkanBuffer`.
//!   4. Allocate an exportable `HostVulkanTimelineSemaphore` via
//!      `device_arc.device()` — verifies the cdylib-reachable
//!      timeline-semaphore path.
//!   5. Construct `CpuReadbackSurfaceAdapter<HostVulkanDevice>` against
//!      the same device.
//!   6. Build `HostSurfaceRegistration<HostMarker>` carrying the
//!      staging buffer + timeline and register the surface.
//!   7. `acquire_write → view_mut → plane_mut → bytes_mut`, write a
//!      sentinel byte, drop the guard.
//!   8. Write the success line to `output_path`.
//!
//! Output format:
//!   - "OK\n<width>x<height>\nbytes_written=<n>" — full round-trip
//!     succeeded.
//!   - "ERR:<message>" — any step failed.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::host_rhi::{
    HostMarker, HostVulkanBuffer, HostVulkanTimelineSemaphore,
};
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
use streamlib_adapter_cpu_readback::{
    CpuReadbackCopyTrigger, CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
    InProcessCpuReadbackCopyTrigger, VulkanLayout,
};

#[streamlib::sdk::processor("CpuReadbackSmokeTestProcessor")]
pub struct CpuReadbackSmokeTest {}

impl ManualProcessor for CpuReadbackSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        const W: u32 = 64;
        const H: u32 = 64;
        const SURFACE_ID: u64 = 1;

        let line = match run_smoke::<W, H>(ctx, SURFACE_ID) {
            Ok(bytes_written) => {
                format!("OK\n{W}x{H}\nbytes_written={bytes_written}")
            }
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "CpuReadbackSmokeTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        std::fs::write(&output_path, "ERR:cpu-readback smoke is linux-only")
            .map_err(|e| {
                Error::Runtime(format!(
                    "CpuReadbackSmokeTest: write {output_path}: {e}"
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
fn run_smoke<const W: u32, const H: u32>(
    ctx: &RuntimeContextFullAccess<'_>,
    surface_id: u64,
) -> Result<usize> {
    ctx.gpu_limited_access().escalate(|full| {
        // Step 1: extract Arc<HostVulkanDevice> via the v9 bridge.
        let host_device = full.host_vulkan_device_arc()?;

        // Step 2: acquire a render-target DMA-BUF VkImage via the
        // existing FullAccess vtable slot, then extract its underlying
        // Arc<HostVulkanTexture> via the v10 host_vulkan_texture_arc
        // bridge. The cpu-readback trigger requires a source VkImage.
        let stream_texture =
            full.acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm)?;
        let texture_arc = stream_texture.host_vulkan_texture_arc()?;

        // Step 3: allocate HOST_VISIBLE staging VkBuffer through the
        // cdylib-reachable constructor.
        let staging_bytes = (W as u64) * (H as u64) * 4u64;
        let staging = HostVulkanBuffer::new(&host_device, staging_bytes)
            .map_err(|e| {
                Error::GpuError(format!("HostVulkanBuffer::new: {e}"))
            })?;
        let staging_arc = Arc::new(staging);

        // Step 4: allocate exportable timeline semaphore through the
        // cdylib-reachable constructor.
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

        // Step 5: construct CpuReadbackSurfaceAdapter against the same
        // device.
        let trigger = Arc::new(InProcessCpuReadbackCopyTrigger::new(
            Arc::clone(&host_device),
        )) as Arc<dyn CpuReadbackCopyTrigger<HostMarker>>;
        let adapter = Arc::new(CpuReadbackSurfaceAdapter::new(
            Arc::clone(&host_device),
            trigger,
        ));

        // Step 6: register the host surface with the adapter.
        adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration::<HostMarker> {
                    texture: Some(Arc::clone(&texture_arc)),
                    staging_planes: vec![Arc::clone(&staging_arc)],
                    timeline: Arc::clone(&timeline_arc),
                    initial_image_layout: VulkanLayout::UNDEFINED,
                    format: SurfaceFormat::Bgra8,
                    width: W,
                    height: H,
                },
            )
            .map_err(|e| {
                Error::GpuError(format!(
                    "CpuReadbackSurfaceAdapter::register_host_surface: {e:?}"
                ))
            })?;

        // Step 7: acquire_write → view_mut → write sentinel → drop guard.
        let surface = StreamlibSurface::new(
            surface_id,
            W,
            H,
            SurfaceFormat::Bgra8,
            SurfaceUsage::CPU_READBACK,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        let mut guard = adapter.acquire_write(&surface).map_err(|e| {
            Error::GpuError(format!(
                "CpuReadbackSurfaceAdapter::acquire_write: {e:?}"
            ))
        })?;
        let bytes_written = {
            let view = guard.view_mut();
            let plane = view.plane_mut(0);
            let bytes = plane.bytes_mut();
            if bytes.is_empty() {
                return Err(Error::GpuError(
                    "plane.bytes_mut() returned empty slice".into(),
                ));
            }
            bytes[0] = 0xAB;
            1usize
        };
        drop(guard);

        // Drop adapter + Arcs explicitly; the staging buffer / timeline
        // hold strong refs to the device_arc through their Drop impls
        // so the device cleanup runs at scope exit.
        drop(adapter);
        drop(timeline_arc);
        drop(staging_arc);
        drop(texture_arc);

        Ok(bytes_written)
    })
}
