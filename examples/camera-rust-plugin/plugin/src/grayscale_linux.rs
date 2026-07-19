// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grayscale processor (Linux).
//!
//! Samples the input camera texture through a sandboxed graphics kernel
//! ([`SandboxedGrayscale`]) and writes a BT.601-luma grayscale frame into
//! a ring of output render-target textures, then forwards the slot's
//! `surface_id` downstream. The in-process `Display` consumer resolves the
//! slot via Path 1 (`texture_cache`), so no surface-share registration is
//! needed — [`GpuContextFullAccess::create_texture_ring`] registers every
//! slot in the texture cache at construction.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use streamlib_plugin_sdk::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{TextureFormat, TextureRing, TextureUsages, VulkanLayout};

use crate::_generated_::VideoFrame;
use crate::grayscale_kernel::{
    GrayscaleInput, GrayscaleInputs, GrayscaleOutput, SandboxedGrayscale,
};

/// Output texture ring depth — the engine's `MAX_FRAMES_IN_FLIGHT = 2`
/// per `docs/learnings/vulkan-frames-in-flight.md`. The prior slot may
/// still be sampled by `Display` while we render into the next one.
const OUTPUT_RING_DEPTH: usize = 2;

struct LinuxBackend {
    kernel: Arc<SandboxedGrayscale>,
    output_ring: TextureRing,
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/camera-rust-plugin/GrayscaleRust",
    execution = reactive,
    input("video_in", "@tatolab/core/VideoFrame"),
    output("video_out", "@tatolab/core/VideoFrame"),
)]
pub struct GrayscaleProcessor {
    gpu_context: Option<GpuContextLimitedAccess>,
    frame_count: AtomicU64,
    backend: Option<LinuxBackend>,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for GrayscaleProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.setup_inner(ctx)
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "GrayscaleProcessor: shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Configuration("GPU context not initialized".into()))?
            .clone();

        let backend = self.backend.as_mut().ok_or_else(|| {
            Error::Configuration("GrayscaleProcessor: backend not initialized".into())
        })?;

        // Resolve input texture + its current_layout. The upstream camera
        // publishes a texture-backed surface_id dual-registered in
        // texture_cache (Path 1) + surface_store (Path 2).
        let input_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;
        let input_texture = input_registration.texture().clone();
        let input_layout = input_registration.current_layout();

        // Rotate to the next ring slot.
        let slot = backend.output_ring.acquire_next();
        let slot_surface_id = slot.surface_id().to_string();

        // Resolve the slot registration so we can update its layout after
        // the dispatch. The ring registered every slot in texture_cache at
        // construction, so Path 1 resolves it without IPC.
        let slot_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &slot_surface_id,
            None,
            slot.texture.width(),
            slot.texture.height(),
        )?;

        backend.kernel.dispatch(GrayscaleInputs {
            input: GrayscaleInput {
                texture: &input_texture,
                current_layout: input_layout,
            },
            output: GrayscaleOutput {
                texture: &slot.texture,
            },
        })?;

        // The kernel leaves both input and output in
        // SHADER_READ_ONLY_OPTIMAL — update both registrations so the next
        // consumer's barrier reads a current_layout matching reality.
        slot_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        input_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

        let output_frame = VideoFrame {
            surface_id: slot_surface_id,
            width: slot.texture.width(),
            height: slot.texture.height(),
            timestamp_ns: frame.timestamp_ns.clone(),
            fps: frame.fps,
            texture_layout: None,
            color_info: frame.color_info.clone(),
            mastering_display: frame.mastering_display.clone(),
            content_light: frame.content_light.clone(),
        };
        self.outputs.write("video_out", &output_frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl GrayscaleProcessor::Processor {
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("GrayscaleProcessor: setup (Vulkan graphics kernel)");
        let gpu_context = ctx.gpu_limited_access().clone();
        self.gpu_context = Some(gpu_context.clone());

        // setup() runs inside the engine's privileged lifecycle dispatch,
        // so `ctx.gpu_full_access()` is already privileged — calling
        // `gpu_limited_access().escalate(...)` here would re-enter the
        // escalate gate on the same thread and trip its re-entry panic.
        let full = ctx.gpu_full_access();
        let kernel = Arc::new(SandboxedGrayscale::new(full)?);

        // Camera in this example emits 1920×1080. The grayscale kernel
        // validates input/output dimensions match per dispatch, so a
        // mismatched frame surfaces as a clean error rather than silently
        // mis-rendering.
        let width = 1920;
        let height = 1080;
        let output_ring = full.create_texture_ring(
            width,
            height,
            TextureFormat::Bgra8Unorm,
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC,
            OUTPUT_RING_DEPTH,
        )?;

        tracing::info!(
            "GrayscaleProcessor: pre-allocated {OUTPUT_RING_DEPTH} output ring slots \
             ({width}x{height} BGRA8)"
        );

        self.backend = Some(LinuxBackend {
            kernel,
            output_ring,
        });
        Ok(())
    }
}
