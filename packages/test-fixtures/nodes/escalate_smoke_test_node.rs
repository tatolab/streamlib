// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::{Input, computed};
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::Texture;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

use crate::result_written_to_its_file::the_result_written_to_its_file;

#[streamlib::sdk::node(
    execution = manual,
    input("output_path", description = "The file the node writes its result to.", value = String, default = String::new()),
)]
#[meta(
    description = "Escalate smoke test fixture — runs wait_device_idle, acquire_pixel_buffer, acquire_output_texture and register_texture_with_layout through FullAccess in its body, and writes the result to the file its output_path names"
)]
pub fn escalate_smoke_test(output_path: Input<String>, ctx: &RuntimeContextFullAccess<'_>) {
    // The body runs once, with FullAccess.
    //
    // Coverage: wait_device_idle, acquire_pixel_buffer,
    // acquire_output_texture, register_texture_with_layout.
    let outcome = run_escalate_smoke(ctx).map(|()| Vec::new());
    computed(move || {
        the_result_written_to_its_file("EscalateSmokeTest", &output_path.get(), &outcome)
    });
}

fn run_escalate_smoke(ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    let full = ctx.gpu_full_access();
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
}
