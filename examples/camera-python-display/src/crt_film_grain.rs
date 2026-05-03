// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CRT + Film Grain Processor
//!
//! Applies vintage CRT display effects and 80s Blade Runner-style film grain:
//! - Barrel distortion (curved screen)
//! - Scanlines with animation
//! - Chromatic aberration (RGB separation)
//! - Vignette (edge darkening)
//! - Heavy animated film grain (moving noise)
//!
//! macOS uses a Metal vertex+fragment pipeline; Linux uses
//! [`streamlib::VulkanCrtFilmGrain`] (a compute kernel).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use streamlib::core::rhi::PixelFormat;
use streamlib::core::{
    GpuContextLimitedAccess, Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
    StreamError,
};
use streamlib::Videoframe;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::core::rhi::RhiTextureCache;

#[cfg(target_os = "linux")]
use std::sync::Arc as StdArc;
#[cfg(target_os = "linux")]
use streamlib::{CrtFilmGrainInputs, VulkanCrtFilmGrain};

// Per-platform GPU backend stash. Defined as a single field on the
// processor (proc-macro `#[streamlib::processor]` strips `#[cfg]` attrs
// from individual fields, so we collapse the cfg into the type alias —
// same pattern as `BlendingCompositorProcessor`).
#[cfg(any(target_os = "macos", target_os = "ios"))]
type GpuBackendStash = Option<MetalState>;
#[cfg(target_os = "linux")]
type GpuBackendStash = Option<StdArc<VulkanCrtFilmGrain>>;
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
type GpuBackendStash = ();

#[cfg(any(target_os = "macos", target_os = "ios"))]
struct MetalState {
    render_pipeline: metal::RenderPipelineState,
    render_pass_desc: metal::RenderPassDescriptor,
    sampler: metal::SamplerState,
    /// Ring buffer of uniform buffers to avoid CPU-GPU sync hazards.
    uniforms_buffers: [metal::Buffer; 3],
    uniforms_index: usize,
    /// Lazily populated on the first `process()` call after setup.
    texture_cache: Option<RhiTextureCache>,
}

/// Uniform buffer for CRT + Film Grain shader (macOS Metal layout).
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[repr(C)]
struct CrtFilmGrainUniforms {
    time: f32,
    crt_curve: f32,
    scanline_intensity: f32,
    chromatic_aberration: f32,
    grain_intensity: f32,
    grain_speed: f32,
    vignette_intensity: f32,
    brightness: f32,
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
            crt_curve: 0.7,              // Noticeable curve
            scanline_intensity: 0.6,     // Visible scanlines
            chromatic_aberration: 0.004, // Subtle RGB separation
            grain_intensity: 0.18,       // Visible but not overwhelming film grain
            grain_speed: 1.0,            // Normal 24fps-style grain flicker
            vignette_intensity: 0.5,     // Medium vignette
            brightness: 2.2,             // Boosted brightness (CRT style)
        }
    }
}

#[streamlib::processor("com.tatolab.crt_film_grain")]
pub struct CrtFilmGrainProcessor {
    config: CrtFilmGrainConfig,
    gpu_context: Option<GpuContextLimitedAccess>,
    frame_count: AtomicU64,
    start_time: Option<Instant>,
    backend: GpuBackendStash,
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
        let width = frame.width;
        let height = frame.height;

        let elapsed = self
            .start_time
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        let input_buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        let (output_pool_id, output_buffer) =
            gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;

        self.process_frame_inner(elapsed, &gpu_ctx, &input_buffer, &output_buffer)?;

        let output_frame = Videoframe {
            surface_id: output_pool_id.to_string(),
            width,
            height,
            timestamp_ns: frame.timestamp_ns.clone(),
            frame_index: frame.frame_index.clone(),
            fps: frame.fps,
            // Per-frame override is opt-in (#633); per-surface
            // `current_image_layout` from surface-share is the default.
            texture_layout: None,
        };
        self.outputs.write("video_out", &output_frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CrtFilmGrainProcessor::Processor {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("CrtFilmGrain: setup (Metal)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        self.start_time = Some(Instant::now());

        let metal_device_ref = ctx.gpu_full_access().device().metal_device_ref();

        let shader_source = include_str!("shaders/crt_film_grain.metal");
        let library = metal_device_ref
            .new_library_with_source(shader_source, &metal::CompileOptions::new())
            .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {e}")))?;

        let vertex_fn = library
            .get_function("crt_vertex", None)
            .map_err(|e| StreamError::Configuration(format!("Vertex not found: {e}")))?;
        let fragment_fn = library
            .get_function("crt_film_grain_fragment", None)
            .map_err(|e| StreamError::Configuration(format!("Fragment not found: {e}")))?;

        let pipeline_desc = metal::RenderPipelineDescriptor::new();
        pipeline_desc.set_vertex_function(Some(&vertex_fn));
        pipeline_desc.set_fragment_function(Some(&fragment_fn));
        pipeline_desc
            .color_attachments()
            .object_at(0)
            .unwrap()
            .set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);

        let render_pipeline = metal_device_ref
            .new_render_pipeline_state(&pipeline_desc)
            .map_err(|e| StreamError::Configuration(format!("Pipeline failed: {e}")))?;

        let sampler_desc = metal::SamplerDescriptor::new();
        sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
        sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
        sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
        sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
        let sampler = metal_device_ref.new_sampler(&sampler_desc);

        let render_pass_desc_ref = metal::RenderPassDescriptor::new();
        let attachment = render_pass_desc_ref
            .color_attachments()
            .object_at(0)
            .unwrap();
        attachment.set_load_action(metal::MTLLoadAction::Clear);
        attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
        attachment.set_store_action(metal::MTLStoreAction::Store);

        let uniforms_size = std::mem::size_of::<CrtFilmGrainUniforms>() as u64;
        let make_uniforms = || {
            metal_device_ref
                .new_buffer(uniforms_size, metal::MTLResourceOptions::CPUCacheModeDefaultCache)
        };
        let uniforms_buffers = [make_uniforms(), make_uniforms(), make_uniforms()];

        self.backend = Some(MetalState {
            render_pipeline,
            render_pass_desc: render_pass_desc_ref.to_owned(),
            sampler,
            uniforms_buffers,
            uniforms_index: 0,
            texture_cache: None,
        });

        tracing::info!(
            "CrtFilmGrain: Initialized (curve={:.1}, scanlines={:.1}, grain={:.2})",
            self.config.crt_curve,
            self.config.scanline_intensity,
            self.config.grain_intensity
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("CrtFilmGrain: setup (Vulkan)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        self.start_time = Some(Instant::now());
        let vulkan_device = ctx.gpu_full_access().device().vulkan_device().clone();
        let kernel = VulkanCrtFilmGrain::new(&vulkan_device)?;
        self.backend = Some(StdArc::new(kernel));

        tracing::info!(
            "CrtFilmGrain: Initialized (curve={:.1}, scanlines={:.1}, grain={:.2})",
            self.config.crt_curve,
            self.config.scanline_intensity,
            self.config.grain_intensity
        );
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    fn setup_inner(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(StreamError::Configuration(
            "CrtFilmGrain: no GPU backend on this platform".into(),
        ))
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn process_frame_inner(
        &mut self,
        elapsed: f32,
        gpu_ctx: &GpuContextLimitedAccess,
        input_buffer: &streamlib::core::rhi::RhiPixelBuffer,
        output_buffer: &streamlib::core::rhi::RhiPixelBuffer,
    ) -> Result<()> {
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("GPU backend not initialized".into()))?;

        if backend.texture_cache.is_none() {
            backend.texture_cache = Some(gpu_ctx.create_texture_cache()?);
        }
        let texture_cache = backend.texture_cache.as_ref().unwrap();

        let input_view = texture_cache.create_view(input_buffer)?;
        let input_metal: &metal::TextureRef = input_view.as_metal_texture();

        let output_view = texture_cache.create_view(output_buffer)?;
        let output_metal: &metal::TextureRef = output_view.as_metal_texture();

        backend
            .render_pass_desc
            .color_attachments()
            .object_at(0)
            .unwrap()
            .set_texture(Some(output_metal));

        let command_queue = gpu_ctx.command_queue().metal_queue_ref();
        let command_buffer = command_queue.new_command_buffer();
        let render_enc = command_buffer.new_render_command_encoder(&backend.render_pass_desc);
        render_enc.set_render_pipeline_state(&backend.render_pipeline);

        render_enc.set_fragment_texture(0, Some(input_metal));
        render_enc.set_fragment_sampler_state(0, Some(&backend.sampler));

        backend.uniforms_index = (backend.uniforms_index + 1) % backend.uniforms_buffers.len();
        let uniforms = &backend.uniforms_buffers[backend.uniforms_index];
        unsafe {
            let ptr = uniforms.contents() as *mut CrtFilmGrainUniforms;
            (*ptr).time = elapsed;
            (*ptr).crt_curve = self.config.crt_curve;
            (*ptr).scanline_intensity = self.config.scanline_intensity;
            (*ptr).chromatic_aberration = self.config.chromatic_aberration;
            (*ptr).grain_intensity = self.config.grain_intensity;
            (*ptr).grain_speed = self.config.grain_speed;
            (*ptr).vignette_intensity = self.config.vignette_intensity;
            (*ptr).brightness = self.config.brightness;
        }

        render_enc.set_fragment_buffer(0, Some(uniforms), 0);
        render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_enc.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn process_frame_inner(
        &mut self,
        elapsed: f32,
        _gpu_ctx: &GpuContextLimitedAccess,
        input_buffer: &streamlib::core::rhi::RhiPixelBuffer,
        output_buffer: &streamlib::core::rhi::RhiPixelBuffer,
    ) -> Result<()> {
        let backend = self
            .backend
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU backend not initialized".into()))?;
        backend.dispatch(CrtFilmGrainInputs {
            input: input_buffer,
            output: output_buffer,
            time_seconds: elapsed,
            crt_curve: self.config.crt_curve,
            scanline_intensity: self.config.scanline_intensity,
            chromatic_aberration: self.config.chromatic_aberration,
            grain_intensity: self.config.grain_intensity,
            grain_speed: self.config.grain_speed,
            vignette_intensity: self.config.vignette_intensity,
            brightness: self.config.brightness,
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    fn process_frame_inner(
        &mut self,
        _elapsed: f32,
        _gpu_ctx: &GpuContextLimitedAccess,
        _input_buffer: &streamlib::core::rhi::RhiPixelBuffer,
        _output_buffer: &streamlib::core::rhi::RhiPixelBuffer,
    ) -> Result<()> {
        Err(StreamError::Configuration(
            "CrtFilmGrain: no GPU backend on this platform".into(),
        ))
    }
}
