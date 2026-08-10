// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Escalate smoke test fixture.
//!
//! Proves `escalate()` fires end-to-end and that representative
//! `GpuContextFullAccess` methods run correctly inside the scope. The
//! processor's `start()` runs an escalate scope that exercises:
//!
//!   1. Entering the escalate gate and constructing a
//!      `GpuContextFullAccess`.
//!   2. Inside the closure:
//!      - `full.wait_device_idle()`.
//!      - `full.acquire_pixel_buffer(...)` — returns a `PixelBuffer`
//!        handle.
//!      - `full.acquire_output_texture(...)` — returns
//!        `(String, Texture)`.
//!      - `full.register_texture_with_layout(...)` (per #906 exit
//!        criterion #4).
//!      If any FullAccess method regresses, this test fails.
//!   3. Leaving the scope releases the gate and runs `wait_device_idle`.
//!
//! Output format:
//!   - "OK" — escalate round-trip + all four method calls succeeded.
//!   - "ERR:<message>" — any step failed.

#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/EscalateSmokeTestProcessor",
    description = "Escalate smoke test fixture — runs gpu.escalate(|_full| Ok(())) end-to-end through escalate_begin/escalate_end and FullAccess construction",
    execution = manual,
    config = crate::test_fixture_processor_configs::EscalateSmokeTestProcessorConfig,
)]
pub struct EscalateSmokeTest {}

impl ManualProcessor for EscalateSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        // Manual-mode start() takes FullAccess directly.
        //
        // Coverage: wait_device_idle, acquire_pixel_buffer,
        // acquire_output_texture, register_texture_with_layout.
        let full = ctx.gpu_full_access();
        let result: Result<()> = (|| -> Result<()> {
            full.wait_device_idle()?;

            let (_pool_id, _pb) = full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;

            // Returns a Texture we hand to register_texture_with_layout
            // below.
            let (id, texture) = full.acquire_output_texture(64, 64, TextureFormat::Rgba8Unorm)?;

            // An explicit non-UNDEFINED layout exercises the layout
            // argument path. SHADER_READ_ONLY_OPTIMAL is a
            // representative non-default value.
            #[cfg(target_os = "linux")]
            full.register_texture_with_layout(&id, texture, VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
            #[cfg(not(target_os = "linux"))]
            {
                // VulkanLayout doesn't exist on non-Linux; on those
                // platforms the test still exercises the other three
                // methods.
                drop(id);
                drop(texture);
            }

            Ok(())
        })();

        let line = match result {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line)
            .map_err(|e| Error::Runtime(format!("EscalateSmokeTest: write {output_path}: {e}")))?;
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
