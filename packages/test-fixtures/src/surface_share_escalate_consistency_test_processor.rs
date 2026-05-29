// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dlopen-cdylib smoke fixture verifying surface-share daemon and
//! escalate-IPC paths agree on resource lifetime for the same
//! `surface_id`.
//!
//! Exercises every leg of the two coordination paths from inside a
//! cdylib's `start()` → `escalate(|full| ...)` closure:
//!
//!   1. Acquire a render-target DMA-BUF VkImage + exportable timeline.
//!   2. Register via `gpu.surface_store().register_texture(...)` —
//!      the cross-process (surface-share daemon) channel.
//!   3. **Dual-register** via `gpu.register_texture_with_layout(...)` —
//!      the in-process `texture_cache` channel (per
//!      `docs/architecture/adapter-runtime-integration.md`'s
//!      "Dual-registration for in-process consumers" recipe).
//!   4. Resolve via `gpu.resolve_texture_registration_by_surface_id(...)`
//!      — the in-process / escalate-IPC resolve path (Path 1 hits the
//!      texture cache; Path 2 would fall back to `lookup_texture` and
//!      QFOT acquire).
//!   5. Look up via `gpu.surface_store().lookup_texture(...)` — the
//!      cross-process channel's symmetric reader.
//!
//! On stop / teardown, both registrations release their hold on the
//! shared Arc<HostVulkanTexture>; the integration test's
//! `runtime.stop()` would surface any double-free as a panic in the
//! test binary teardown.
//!
//! Output:
//!   - "OK\nregister_texture_with_layout=ok\nsurface_store_register_texture=ok\nresolve_texture_registration=ok\nsurface_store_lookup_texture=ok"
//!   - "ERR:<msg>" on any step failure.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[cfg(target_os = "linux")]
use streamlib::sdk::engine::host_rhi::HostVulkanTimelineSemaphore;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::HostSurfaceStoreExt;
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::TextureFormat;
#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;

#[streamlib::sdk::processor("SurfaceShareEscalateConsistencyTestProcessor")]
pub struct SurfaceShareEscalateConsistencyTest {}

impl ManualProcessor for SurfaceShareEscalateConsistencyTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        const W: u32 = 64;
        const H: u32 = 64;
        const SURFACE_ID: &str = "surface-share-escalate-smoke";

        let line = match run_smoke(ctx, SURFACE_ID, W, H) {
            Ok(()) => "OK\n\
                       register_texture_with_layout=ok\n\
                       surface_store_register_texture=ok\n\
                       resolve_texture_registration=ok\n\
                       surface_store_lookup_texture=ok"
                .to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "SurfaceShareEscalateConsistencyTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        std::fs::write(
            &output_path,
            "ERR:surface-share+escalate consistency smoke is linux-only",
        )
        .map_err(|e| {
            Error::Runtime(format!(
                "SurfaceShareEscalateConsistencyTest: write {output_path}: {e}"
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
    surface_id: &str,
    width: u32,
    height: u32,
) -> Result<()> {
    // Manual-mode start() takes FullAccess directly; the engine
    // wraps cdylib lifecycle dispatch in `with_cdylib_scope` (#1075),
    // so `ctx.gpu_full_access()` is `ScopeToken`-flavored and
    // dispatches through the FullAccess vtable transparently.
    // Same coverage as the pre-#1075 escalate path; the wrap is the
    // engine-side replacement for the explicit `.escalate(|full|...)`.
    let full = ctx.gpu_full_access();

    // Step 1: allocate texture + exportable timeline pair (the resource
    // triple both registration paths bind to). Single-writer-per-edge:
    // `produce_done` + `consume_done` per
    // `docs/architecture/adapter-timeline-single-writer.md`.
    let host_device = full.host_vulkan_device_arc()?;
    let texture = full.acquire_render_target_dma_buf_image(
        width,
        height,
        TextureFormat::Bgra8Unorm,
    )?;
    let produce_done = HostVulkanTimelineSemaphore::new_exportable(
        host_device.device(),
        0,
    )
    .map_err(|e| {
        Error::GpuError(format!(
            "HostVulkanTimelineSemaphore::new_exportable (produce_done): {e}"
        ))
    })?;
    let consume_done = HostVulkanTimelineSemaphore::new_exportable(
        host_device.device(),
        0,
    )
    .map_err(|e| {
        Error::GpuError(format!(
            "HostVulkanTimelineSemaphore::new_exportable (consume_done): {e}"
        ))
    })?;

    // Step 2: register via the cross-process (surface-share daemon)
    // channel.
    let store = full
        .surface_store()
        .ok_or_else(|| Error::GpuError("surface_store unavailable".into()))?;
    store
        .register_texture(
            surface_id,
            &texture,
            Some(&produce_done),
            Some(&consume_done),
            VulkanLayout::UNDEFINED,
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "surface_store.register_texture: {e}"
            ))
        })?;

    // Step 3: dual-register via the in-process texture_cache
    // channel so the Path-1 resolve below hits the fast path.
    // Cloning the Texture PluginAbiObject bumps the underlying
    // Arc<TextureInner> via the LimitedAccess `clone_texture` slot.
    let texture_cache_clone = texture.clone();
    full.register_texture_with_layout(
        surface_id,
        texture_cache_clone,
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    );

    // Step 4: resolve via the in-process / escalate-IPC path.
    // Returns a TextureRegistration handle the cdylib never mutates;
    // we just verify the call succeeds. `texture_layout=None`
    // tells the resolver to read the producer's published layout
    // from the registration rather than override per-frame.
    let _registration = full
        .resolve_texture_registration_by_surface_id(
            surface_id,
            None,
            width,
            height,
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "resolve_texture_registration_by_surface_id: {e}"
            ))
        })?;

    // Step 5: look up via the cross-process (surface-share daemon)
    // path. Returns a fresh Texture PluginAbiObject over the same imported
    // VkImage. Both texture handles release on scope exit; the
    // host-side Arcs drop in inverse-construction order.
    let (looked_up_texture, looked_up_layout) =
        store.lookup_texture(surface_id).map_err(|e| {
            Error::GpuError(format!(
                "surface_store.lookup_texture: {e}"
            ))
        })?;
    // Sanity: the looked-up texture's dimensions must agree with
    // the original — locks the dimensions half of the round-trip
    // wire format.
    if looked_up_texture.width() != width
        || looked_up_texture.height() != height
    {
        return Err(Error::GpuError(format!(
            "lookup_texture returned mismatched dimensions: \
             expected {width}x{height}, got {}x{}",
            looked_up_texture.width(),
            looked_up_texture.height()
        )));
    }
    // And the layout — locks the layout half of the wire format.
    // The producer registered `VulkanLayout::UNDEFINED`; the
    // daemon round-trips the raw `i32` enumerant through msgpack
    // and the consumer-side wrapper decodes it back.
    if looked_up_layout != VulkanLayout::UNDEFINED {
        return Err(Error::GpuError(format!(
            "lookup_texture returned mismatched layout: \
             expected UNDEFINED (0), got {:?}",
            looked_up_layout
        )));
    }

    // All five legs succeeded. Drop everything explicitly so the
    // host-side Arc refcounts drain before returning rather than at
    // processor stop.
    drop(looked_up_texture);
    drop(_registration);
    drop(texture);
    drop(produce_done);
    drop(consume_done);
    drop(host_device);

    Ok(())
}
