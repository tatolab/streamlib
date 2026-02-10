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

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use streamlib::core::rhi::{PixelFormat, RhiTextureCache};
use streamlib::core::{GpuContext, Result, RuntimeContext, StreamError};
use streamlib::Videoframe;

/// Uniform buffer for CRT + Film Grain shader.
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

    gpu_context: Option<GpuContext>,
    render_pipeline: Option<metal::RenderPipelineState>,
    render_pass_desc: Option<metal::RenderPassDescriptor>,
    sampler: Option<metal::SamplerState>,
    /// Ring buffer of uniform buffers to avoid CPU-GPU sync hazards.
    uniforms_buffers: [Option<metal::Buffer>; 3],
    uniforms_index: usize,
    frame_count: AtomicU64,
    start_time: Option<Instant>,

    // RHI resources
    texture_cache: Option<RhiTextureCache>,
}

impl streamlib::core::ReactiveProcessor for CrtFilmGrainProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::info!("CrtFilmGrain: Setting up...");

            self.gpu_context = Some(ctx.gpu.clone());
            self.start_time = Some(Instant::now());
            self.texture_cache = None; // Deferred to first process()

            let metal_device_ref = ctx.gpu.device().metal_device_ref();

            // Compile shaders
            let shader_source = include_str!("shaders/crt_film_grain.metal");
            let library = metal_device_ref
                .new_library_with_source(shader_source, &metal::CompileOptions::new())
                .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {}", e)))?;

            let vertex_fn = library
                .get_function("crt_vertex", None)
                .map_err(|e| StreamError::Configuration(format!("Vertex not found: {}", e)))?;
            let fragment_fn = library
                .get_function("crt_film_grain_fragment", None)
                .map_err(|e| StreamError::Configuration(format!("Fragment not found: {}", e)))?;

            // Render pipeline
            let pipeline_desc = metal::RenderPipelineDescriptor::new();
            pipeline_desc.set_vertex_function(Some(&vertex_fn));
            pipeline_desc.set_fragment_function(Some(&fragment_fn));

            let color_attachment = pipeline_desc.color_attachments().object_at(0).unwrap();
            color_attachment.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);

            let render_pipeline = metal_device_ref
                .new_render_pipeline_state(&pipeline_desc)
                .map_err(|e| StreamError::Configuration(format!("Pipeline failed: {}", e)))?;

            // Sampler with linear filtering
            let sampler_desc = metal::SamplerDescriptor::new();
            sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
            sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
            let sampler = metal_device_ref.new_sampler(&sampler_desc);

            // Create render pass descriptor
            let render_pass_desc_ref = metal::RenderPassDescriptor::new();
            let color_attachment = render_pass_desc_ref
                .color_attachments()
                .object_at(0)
                .unwrap();
            color_attachment.set_load_action(metal::MTLLoadAction::Clear);
            color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);

            // Uniforms ring buffer (3 buffers)
            let uniforms_size = std::mem::size_of::<CrtFilmGrainUniforms>() as u64;
            let uniforms_buffers = [
                Some(metal_device_ref.new_buffer(
                    uniforms_size,
                    metal::MTLResourceOptions::CPUCacheModeDefaultCache,
                )),
                Some(metal_device_ref.new_buffer(
                    uniforms_size,
                    metal::MTLResourceOptions::CPUCacheModeDefaultCache,
                )),
                Some(metal_device_ref.new_buffer(
                    uniforms_size,
                    metal::MTLResourceOptions::CPUCacheModeDefaultCache,
                )),
            ];

            self.render_pipeline = Some(render_pipeline);
            self.render_pass_desc = Some(render_pass_desc_ref.to_owned());
            self.sampler = Some(sampler);
            self.uniforms_buffers = uniforms_buffers;
            self.uniforms_index = 0;

            tracing::info!(
                "CrtFilmGrain: Initialized (curve={:.1}, scanlines={:.1}, grain={:.2})",
                self.config.crt_curve,
                self.config.scanline_intensity,
                self.config.grain_intensity
            );
            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "CrtFilmGrain: Shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
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
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Lazy texture cache creation
        if self.texture_cache.is_none() {
            self.texture_cache = Some(gpu_ctx.create_texture_cache()?);
        }
        let texture_cache = self.texture_cache.as_ref().unwrap();

        let input_buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        let input_view = texture_cache.create_view(&input_buffer)?;
        let input_metal: &metal::TextureRef = input_view.as_metal_texture();

        let command_queue = gpu_ctx.command_queue().metal_queue_ref();

        // Acquire output buffer from GpuContext pool
        let (output_pool_id, output_buffer) =
            gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;
        let output_view = texture_cache.create_view(&output_buffer)?;
        let output_metal: &metal::TextureRef = output_view.as_metal_texture();

        // Update render pass attachment
        let render_pass_desc = self.render_pass_desc.as_ref().unwrap();
        render_pass_desc
            .color_attachments()
            .object_at(0)
            .unwrap()
            .set_texture(Some(output_metal));

        // Render
        let command_buffer = command_queue.new_command_buffer();
        let render_enc = command_buffer.new_render_command_encoder(render_pass_desc);
        render_enc.set_render_pipeline_state(self.render_pipeline.as_ref().unwrap());

        // Bind input texture
        render_enc.set_fragment_texture(0, Some(input_metal));
        render_enc.set_fragment_sampler_state(0, Some(self.sampler.as_ref().unwrap()));

        // Rotate to next uniform buffer
        self.uniforms_index = (self.uniforms_index + 1) % 3;
        let current_uniforms = self.uniforms_buffers[self.uniforms_index].as_ref().unwrap();

        // Update uniforms
        unsafe {
            let ptr = current_uniforms.contents() as *mut CrtFilmGrainUniforms;
            (*ptr).time = elapsed;
            (*ptr).crt_curve = self.config.crt_curve;
            (*ptr).scanline_intensity = self.config.scanline_intensity;
            (*ptr).chromatic_aberration = self.config.chromatic_aberration;
            (*ptr).grain_intensity = self.config.grain_intensity;
            (*ptr).grain_speed = self.config.grain_speed;
            (*ptr).vignette_intensity = self.config.vignette_intensity;
            (*ptr).brightness = self.config.brightness;
        }

        render_enc.set_fragment_buffer(0, Some(current_uniforms), 0);
        render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_enc.end_encoding();

        command_buffer.commit();
        command_buffer.wait_until_completed();

        // Output frame
        let output_frame = Videoframe {
            surface_id: output_pool_id.to_string(),
            width,
            height,
            timestamp_ns: frame.timestamp_ns.clone(),
            frame_index: frame.frame_index.clone(),
        };
        self.outputs.write("video_out", &output_frame)?;

        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
