// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CRT + Film Grain Processor (Linux only).
//!
//! Applies vintage CRT display effects and 80s Blade Runner-style film
//! grain via a sandboxed graphics kernel:
//! - Barrel distortion (curved screen)
//! - Scanlines with animation
//! - Chromatic aberration (RGB separation)
//! - Vignette (edge darkening)
//! - Heavy animated film grain (moving noise)
//!
//! Pre-#487 this processor cross-compiled with a macOS Metal vertex+
//! fragment path; the pre-#485 macOS pipeline could not consume the
//! tiled DMA-BUF VkImages every modern producer in this example
//! emits, so the macOS path was struck wholesale and the processor
//! is now Linux-only. The kernel wrapper itself
//! ([`SandboxedCrtFilmGrain`]) lives in `crt_film_grain_kernel.rs` —
//! see that file's module-level doc for the transitional rationale
//! and the migration path to RDG (#631).

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use streamlib::core::rhi::{StreamTexture, TextureFormat, VulkanLayout};
use streamlib::core::{
    GpuContextLimitedAccess, Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
    StreamError,
};
use streamlib::Videoframe;

use crate::crt_film_grain_kernel::{
    CrtFilmGrainInput, CrtFilmGrainInputs, CrtFilmGrainOutput, SandboxedCrtFilmGrain,
};

/// Output texture ring depth — mirrors `BlendingCompositor` and the
/// engine's `MAX_FRAMES_IN_FLIGHT = 2` per
/// `docs/learnings/vulkan-frames-in-flight.md`.
const OUTPUT_RING_DEPTH: usize = 2;

/// Stable per-slot UUIDs registered in `texture_cache` + `surface_store`.
/// `c20c` ≈ "crt"; last octet = slot index for log correlation.
const CRT_OUTPUT_SURFACE_UUIDS: [&str; OUTPUT_RING_DEPTH] = [
    "00000000-0000-0000-0000-00000c20c000",
    "00000000-0000-0000-0000-00000c20c001",
];

/// Output ring slot — pre-allocated render-target texture + its
/// surface_id (the UUID it is dual-registered under).
struct OutputSlot {
    surface_id: String,
    texture: StreamTexture,
}

struct LinuxBackend {
    kernel: Arc<SandboxedCrtFilmGrain>,
    output_ring: Vec<OutputSlot>,
    next_slot: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrtFilmGrainConfig {
    /// CRT barrel distortion amount (0.0 = flat, 1.0 = heavy curve).
    pub crt_curve: f32,
    /// Scanline darkness intensity (0.0 = none, 1.0 = heavy).
    pub scanline_intensity: f32,
    /// Chromatic aberration / RGB separation (0.0 = none, 0.01 = heavy).
    pub chromatic_aberration: f32,
    /// Film grain intensity (0.0 = none, 1.0 = very heavy).
    pub grain_intensity: f32,
    /// Film grain animation speed (1.0 = normal, 2.0 = fast).
    pub grain_speed: f32,
    /// Vignette (edge darkening) intensity (0.0 = none, 1.0 = heavy).
    pub vignette_intensity: f32,
    /// Overall brightness multiplier.
    pub brightness: f32,
}

impl Default for CrtFilmGrainConfig {
    fn default() -> Self {
        // 80s Blade Runner look
        Self {
            crt_curve: 0.7,
            scanline_intensity: 0.6,
            chromatic_aberration: 0.004,
            grain_intensity: 0.18,
            grain_speed: 1.0,
            vignette_intensity: 0.5,
            brightness: 2.2,
        }
    }
}

#[streamlib::processor("com.tatolab.crt_film_grain")]
pub struct CrtFilmGrainProcessor {
    config: CrtFilmGrainConfig,
    gpu_context: Option<GpuContextLimitedAccess>,
    frame_count: AtomicU64,
    start_time: Option<Instant>,
    backend: Option<LinuxBackend>,
}

impl streamlib::core::ReactiveProcessor for CrtFilmGrainProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = self.setup_inner(ctx);
        std::future::ready(result)
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "CrtFilmGrain: Shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        let elapsed = self
            .start_time
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        let backend = self.backend.as_mut().ok_or_else(|| {
            StreamError::Configuration("CrtFilmGrain: backend not initialized".into())
        })?;

        // Resolve input texture + its current_layout via Path 1 / Path 2
        // (the upstream BlendingCompositor publishes a texture-backed
        // surface_id dual-registered in texture_cache + surface_store).
        let input_registration = gpu_ctx.resolve_videoframe_registration(&frame)?;
        let input_texture = input_registration.texture().clone();
        let input_layout = input_registration.current_layout();

        // Pick the next ring slot. With ring depth = 2, slots alternate
        // every frame; the prior slot may still be sampled by Glitch
        // / Display, but we move to the next one.
        let slot_idx = backend.next_slot;
        backend.next_slot = (slot_idx + 1) % backend.output_ring.len();
        let slot = &backend.output_ring[slot_idx];

        // Read the slot's current_layout from its registration. After
        // the kernel's dispatch this becomes SHADER_READ_ONLY_OPTIMAL;
        // first dispatch reads UNDEFINED.
        let slot_videoframe = synth_slot_videoframe(
            &slot.surface_id,
            slot.texture.width(),
            slot.texture.height(),
        );
        let slot_registration = gpu_ctx.resolve_videoframe_registration(&slot_videoframe)?;
        let slot_current_layout = slot_registration.current_layout();

        backend.kernel.dispatch(CrtFilmGrainInputs {
            input: CrtFilmGrainInput {
                texture: &input_texture,
                current_layout: input_layout,
            },
            output: CrtFilmGrainOutput {
                texture: &slot.texture,
                current_layout: slot_current_layout,
            },
            time_seconds: elapsed,
            crt_curve: self.config.crt_curve,
            scanline_intensity: self.config.scanline_intensity,
            chromatic_aberration: self.config.chromatic_aberration,
            grain_intensity: self.config.grain_intensity,
            grain_speed: self.config.grain_speed,
            vignette_intensity: self.config.vignette_intensity,
            brightness: self.config.brightness,
        })?;

        // Kernel leaves both input and output in SHADER_READ_ONLY_OPTIMAL —
        // update the slot registration so the next consumer's barrier
        // reads a current_layout matching reality.
        slot_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        input_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

        let output_frame = Videoframe {
            surface_id: slot.surface_id.clone(),
            width: slot.texture.width(),
            height: slot.texture.height(),
            timestamp_ns: frame.timestamp_ns.clone(),
            frame_index: frame.frame_index.clone(),
            fps: frame.fps,
            // Per-frame override is opt-in (#633); the per-surface
            // `current_image_layout` published via surface-share is
            // the default.
            texture_layout: None,
        };
        self.outputs.write("video_out", &output_frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CrtFilmGrainProcessor::Processor {
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("CrtFilmGrain: setup (Vulkan graphics kernel)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        self.start_time = Some(Instant::now());

        let gpu_full = ctx.gpu_full_access();
        let vulkan_device = gpu_full.device().vulkan_device().clone();
        let kernel = Arc::new(SandboxedCrtFilmGrain::new(&vulkan_device)?);

        // Pre-allocate the output texture ring — render-target-capable
        // tiled DMA-BUF VkImages, dual-registered (texture_cache for
        // in-process Path 1 consumers like Display; surface_store for
        // cross-process consumers like the cyberpunk_glitch Python
        // subprocess from #486 reaching the ring via
        // `OpenGLContext.acquire_read`). Mirrors the
        // `BlendingCompositor` ring shape exactly — the engine-wide
        // anti-pattern #2 in `texture-registration.md` warns against
        // descriptor-side claims that diverge from registration; we
        // keep both registrations honest.
        let mut output_ring: Vec<OutputSlot> = Vec::with_capacity(OUTPUT_RING_DEPTH);
        let surface_store = gpu_full.surface_store().ok_or_else(|| {
            StreamError::Configuration(
                "CrtFilmGrain: GpuContext has no surface_store \
                 — cross-process output (Glitch consumer, #486) unavailable"
                    .into(),
            )
        })?;
        // Match the example pipeline's 1920x1080 — the upstream
        // BlendingCompositor produces 1920x1080 output.
        let width = 1920u32;
        let height = 1080u32;
        for slot_idx in 0..OUTPUT_RING_DEPTH {
            let texture = gpu_full.acquire_render_target_dma_buf_image(
                width,
                height,
                TextureFormat::Bgra8Unorm,
            )?;
            let surface_id = CRT_OUTPUT_SURFACE_UUIDS[slot_idx].to_string();
            // Initial layout UNDEFINED — first dispatch's pre-render
            // barrier transitions UNDEFINED → COLOR_ATTACHMENT_OPTIMAL,
            // and the post-render barrier ends in
            // SHADER_READ_ONLY_OPTIMAL. From the second dispatch
            // forward the registration carries SHADER_READ_ONLY_OPTIMAL.
            gpu_full.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            // Cross-process registration — declare the steady-state
            // post-render layout (SHADER_READ_ONLY_OPTIMAL). Glitch
            // reads after the first dispatch lands.
            surface_store
                .register_texture(
                    &surface_id,
                    &texture,
                    None,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                )
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "CrtFilmGrain: surface_store.register_texture slot {slot_idx}: {e}"
                    ))
                })?;
            output_ring.push(OutputSlot {
                surface_id,
                texture,
            });
        }

        tracing::info!(
            "CrtFilmGrain: pre-allocated {OUTPUT_RING_DEPTH} output ring slots ({width}x{height} BGRA8) — \
             curve={:.1}, scanlines={:.1}, grain={:.2}",
            self.config.crt_curve,
            self.config.scanline_intensity,
            self.config.grain_intensity
        );

        self.backend = Some(LinuxBackend {
            kernel,
            output_ring,
            next_slot: 0,
        });
        Ok(())
    }
}

/// Synthesize a Videoframe pointing at one of our output ring slots —
/// used to look up its registration for layout reads. The slot is
/// registered at setup time, so Path 1 resolves it without IPC.
fn synth_slot_videoframe(surface_id: &str, width: u32, height: u32) -> Videoframe {
    Videoframe {
        surface_id: surface_id.to_string(),
        width,
        height,
        timestamp_ns: "0".into(),
        frame_index: "0".into(),
        fps: None,
        texture_layout: None,
    }
}

