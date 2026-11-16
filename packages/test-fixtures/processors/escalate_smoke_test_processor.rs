// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::Texture;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

#[streamlib::sdk::processor(
    execution = manual,
    config = crate::test_fixture_processor_configs::EscalateSmokeTestProcessorConfig,
)]
#[meta(
    description = "Escalate smoke test fixture — runs gpu.escalate(|_full| Ok(())) end-to-end through escalate_begin/escalate_end and FullAccess construction"
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
            full.register_texture_with_layout(
                &id,
                Texture::clone(&texture),
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            );
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
