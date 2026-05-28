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
//! Linux-only — the tiled DMA-BUF `VkImage`s every modern producer in
//! this example emits aren't consumable by a macOS Metal vertex+
//! fragment path. The kernel wrapper itself ([`SandboxedCrtFilmGrain`])
//! lives in `crt_film_grain_kernel.rs` — see that file's module-level
//! doc for why it lives in the example and not the engine.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use streamlib::sdk::engine::HostSurfaceStoreExt;

use streamlib::sdk::rhi::{Texture, TextureFormat, VulkanLayout};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Result, Error};
use crate::_generated_::VideoFrame;

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
    texture: Texture,
}

struct LinuxBackend {
    kernel: Arc<SandboxedCrtFilmGrain>,
    output_ring: Vec<OutputSlot>,
    next_slot: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrtFilmGrainConfig {
    /// Output ring slot width in pixels. Must match the upstream
    /// (`BlendingCompositor`) output width.
    pub width: u32,
    /// Output ring slot height in pixels. Must match the upstream
    /// output height.
    pub height: u32,
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
            width: 1920,
            height: 1080,
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

#[streamlib::sdk::processor("CrtFilmGrain")]
pub struct CrtFilmGrainProcessor {
    config: CrtFilmGrainConfig,
    gpu_context: Option<GpuContextLimitedAccess>,
    frame_count: AtomicU64,
    start_time: Option<Instant>,
    backend: Option<LinuxBackend>,
}

impl streamlib::sdk::processors::ReactiveProcessor for CrtFilmGrainProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.setup_inner(ctx)
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "CrtFilmGrain: Shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;

        let elapsed = self
            .start_time
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Configuration("GPU context not initialized".into()))?
            .clone();

        let backend = self.backend.as_mut().ok_or_else(|| {
            Error::Configuration("CrtFilmGrain: backend not initialized".into())
        })?;

        // Resolve input texture + its current_layout via Path 1 / Path 2
        // (the upstream BlendingCompositor publishes a texture-backed
        // surface_id dual-registered in texture_cache + surface_store).
        let input_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;
        let input_texture = input_registration.texture().clone();
        let input_layout = input_registration.current_layout();

        // Pick the next ring slot. With ring depth = 2, slots alternate
        // every frame; the prior slot may still be sampled by Glitch
        // / Display, but we move to the next one.
        let slot_idx = backend.next_slot;
        backend.next_slot = (slot_idx + 1) % backend.output_ring.len();
        let slot = &backend.output_ring[slot_idx];

        // Resolve the slot's registration so we can `update_layout`
        // after the dispatch returns. The kernel's `offscreen_render`
        // starts from `UNDEFINED` internally (content discard
        // permitted, full-screen triangle overwrites every pixel), so
        // it doesn't read the slot's prior layout — we just need the
        // registration handle.
        let slot_videoframe = synth_slot_videoframe(
            &slot.surface_id,
            slot.texture.width(),
            slot.texture.height(),
        );
        let slot_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &slot_videoframe.surface_id,
            slot_videoframe.texture_layout,
            slot_videoframe.width,
            slot_videoframe.height,
        )?;

        backend.kernel.dispatch(CrtFilmGrainInputs {
            input: CrtFilmGrainInput {
                texture: &input_texture,
                current_layout: input_layout,
            },
            output: CrtFilmGrainOutput { texture: &slot.texture },
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

        let output_frame = VideoFrame {
            surface_id: slot.surface_id.clone(),
            width: slot.texture.width(),
            height: slot.texture.height(),
            timestamp_ns: frame.timestamp_ns.clone(),
            fps: frame.fps,
            // Per-frame override is opt-in; the per-surface
            // `current_image_layout` published via surface-share is
            // the default.
            texture_layout: None,
            // Pass through input color metadata — the CRT effect
            // doesn't change the source's primaries/transfer/matrix.
            color_info: frame.color_info.clone(),
            mastering_display: frame.mastering_display.clone(),
            content_light: frame.content_light.clone(),
        };
        self.outputs.write("video_out", &output_frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CrtFilmGrainProcessor::Processor {
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("CrtFilmGrain: setup (Vulkan graphics kernel)");
        let gpu_context = ctx.gpu_limited_access().clone();
        self.gpu_context = Some(gpu_context.clone());
        self.start_time = Some(Instant::now());

        // setup() runs inside the engine's privileged lifecycle dispatch
        // (`ProcessorInstance::setup`), so `ctx.gpu_full_access()` is
        // already privileged in both cdylib and in-process modes: cdylib
        // bodies see a ScopeToken-shaped FullAccess routed through the
        // FullAccess vtable, in-process bodies see the Boxed FullAccess
        // dispatched directly. Calling `gpu_limited_access().escalate(...)`
        // here would re-enter the same gate and trip the gate's same-
        // thread re-entry panic.
        let width = self.config.width;
        let height = self.config.height;
        let full = ctx.gpu_full_access();
        let vulkan_device = full.host_vulkan_device_arc()?;
        let kernel = Arc::new(SandboxedCrtFilmGrain::new(&vulkan_device)?);

        let mut ring_descriptors: Vec<(String, Texture)> =
            Vec::with_capacity(OUTPUT_RING_DEPTH);
        for slot_idx in 0..OUTPUT_RING_DEPTH {
            let texture = full.acquire_render_target_dma_buf_image(
                width,
                height,
                TextureFormat::Bgra8Unorm,
            )?;
            let surface_id = CRT_OUTPUT_SURFACE_UUIDS[slot_idx].to_string();
            ring_descriptors.push((surface_id, texture));
        }

        // Dual-register each slot outside `escalate`:
        // - `GpuContext::texture_cache` (Path 1 — in-process consumers
        //   like `Display`).
        // - `surface_store` (Path 2 — cross-process consumers like the
        //   `cyberpunk_glitch` Python subprocess reaching the ring via
        //   `OpenGLContext.acquire_read`).
        //
        // Path 1 starts at `UNDEFINED` (the kernel's pre-render barrier
        // handles `UNDEFINED → COLOR_ATTACHMENT_OPTIMAL`); Path 2
        // declares `SHADER_READ_ONLY_OPTIMAL` because Glitch reads
        // after the first dispatch lands.
        let mut output_ring: Vec<OutputSlot> = Vec::with_capacity(OUTPUT_RING_DEPTH);
        for (slot_idx, (surface_id, texture)) in ring_descriptors.into_iter().enumerate()
        {
            gpu_context.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            let surface_store = gpu_context.surface_store().ok_or_else(|| {
                Error::Configuration(
                    "CrtFilmGrain: GpuContext has no surface_store \
                     — cross-process output (Glitch consumer) unavailable"
                        .into(),
                )
            })?;
            surface_store
                .register_texture(
                    &surface_id,
                    &texture,
                    None,
                    None,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                )
                .map_err(|e| {
                    Error::Configuration(format!(
                        "CrtFilmGrain: surface_store.register_texture slot {slot_idx}: {e}"
                    ))
                })?;
            output_ring.push(OutputSlot { surface_id, texture });
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

/// Synthesize a VideoFrame pointing at one of our output ring slots —
/// used to look up its registration for layout reads. The slot is
/// registered at setup time, so Path 1 resolves it without IPC.
fn synth_slot_videoframe(surface_id: &str, width: u32, height: u32) -> VideoFrame {
    VideoFrame {
        surface_id: surface_id.to_string(),
        width,
        height,
        timestamp_ns: "0".into(),
        fps: None,
        texture_layout: None,
        color_info: None,
        mastering_display: None,
        content_light: None,
    }
}

