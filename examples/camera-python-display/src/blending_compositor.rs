// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Blending Compositor - Multi-layer alpha blending with PiP support.
//!
//! Composites multiple video layers using Photoshop-style alpha blending:
//! - Layer 1 (base): Video from camera
//! - Layer 2 (middle): Lower third overlay (RGBA with transparency)
//! - Layer 3: Watermark overlay (RGBA with transparency)
//! - Layer 4 (top): PiP overlay with slide-in animation (Breaking News style)
//!
//! The PiP slides in from the right when the avatar processor signals ready.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use streamlib::core::rhi::{PixelFormat, RhiTextureCache, RhiTextureView};
use streamlib::core::{GpuContext, Result, RuntimeContext, StreamError};
use streamlib::Videoframe;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlendingCompositorConfig {
    /// Default output width (used until video arrives)
    pub width: u32,
    /// Default output height (used until video arrives)
    pub height: u32,
    /// Duration of PiP slide-in animation in seconds
    pub pip_slide_duration: f32,
    /// Delay in seconds after first camera frame before PiP slides in
    pub pip_slide_delay: f32,
}

impl Default for BlendingCompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            pip_slide_duration: 0.5, // 500ms slide-in
            pip_slide_delay: 2.5,    // 2.5s after camera starts
        }
    }
}

/// Uniform buffer for blending compositor shader.
/// Must match the Metal shader struct layout exactly.
#[repr(C)]
struct BlendingUniforms {
    has_video: u32,
    has_lower_third: u32,
    has_watermark: u32,
    has_pip: u32,
    pip_slide_progress: f32, // 0.0 = off-screen, 1.0 = fully visible
    _padding1: f32,
    _padding2: f32,
    _padding3: f32,
}

#[streamlib::processor("src/blending_compositor.yaml")]
pub struct BlendingCompositorProcessor {
    config: BlendingCompositorConfig,

    gpu_context: Option<GpuContext>,
    render_pipeline: Option<metal::RenderPipelineState>,
    render_pass_desc: Option<metal::RenderPassDescriptor>,
    sampler: Option<metal::SamplerState>,
    uniforms_buffers: [Option<metal::Buffer>; 3],
    uniforms_index: usize,
    frame_count: AtomicU64,

    // RHI resources
    texture_cache: Option<RhiTextureCache>,

    // Cached texture views
    cached_video_view: Option<RhiTextureView>,
    cached_lower_third_view: Option<RhiTextureView>,
    cached_watermark_view: Option<RhiTextureView>,
    cached_pip_view: Option<RhiTextureView>,
    cached_video_dimensions: Option<(u32, u32)>,

    // PiP animation state
    pip_ready: bool,
    pip_animation_start: Option<Instant>,
    first_video_time: Option<Instant>,
    pip_placeholder_texture: Option<metal::Texture>,

    // Debug timing
    last_frame_time: Option<Instant>,
}

impl streamlib::core::ReactiveProcessor for BlendingCompositorProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::info!("BlendingCompositor: Setting up (reactive mode)...");

            self.gpu_context = Some(ctx.gpu.clone());
            self.texture_cache = None;

            let metal_device_ref = ctx.gpu.device().metal_device_ref();

            // Compile shaders
            let shader_source = include_str!("shaders/blending_compositor.metal");
            let library = metal_device_ref
                .new_library_with_source(shader_source, &metal::CompileOptions::new())
                .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {}", e)))?;

            let vertex_fn = library
                .get_function("blending_vertex", None)
                .map_err(|e| StreamError::Configuration(format!("Vertex not found: {}", e)))?;
            let fragment_fn = library
                .get_function("blending_fragment", None)
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

            // Sampler
            let sampler_desc = metal::SamplerDescriptor::new();
            sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
            sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
            let sampler = metal_device_ref.new_sampler(&sampler_desc);

            // Render pass descriptor
            let render_pass_desc_ref = metal::RenderPassDescriptor::new();
            let color_attachment = render_pass_desc_ref
                .color_attachments()
                .object_at(0)
                .unwrap();
            color_attachment.set_load_action(metal::MTLLoadAction::Clear);
            color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);

            // Uniforms ring buffer
            let uniforms_size = std::mem::size_of::<BlendingUniforms>() as u64;
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

            // Create 1x1 transparent placeholder texture for PiP
            // Allows the PiP frame (border, title bar) to slide in before avatar frames arrive
            let pip_placeholder_desc = metal::TextureDescriptor::new();
            pip_placeholder_desc.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
            pip_placeholder_desc.set_width(1);
            pip_placeholder_desc.set_height(1);
            pip_placeholder_desc.set_usage(metal::MTLTextureUsage::ShaderRead);
            let pip_placeholder = metal_device_ref.new_texture(&pip_placeholder_desc);
            let zero_data: [u8; 4] = [0, 0, 0, 0];
            pip_placeholder.replace_region(
                metal::MTLRegion::new_2d(0, 0, 1, 1),
                0,
                zero_data.as_ptr() as *const std::ffi::c_void,
                4,
            );

            self.render_pipeline = Some(render_pipeline);
            self.render_pass_desc = Some(render_pass_desc_ref.to_owned());
            self.sampler = Some(sampler);
            self.uniforms_buffers = uniforms_buffers;
            self.uniforms_index = 0;
            self.pip_ready = false;
            self.pip_animation_start = None;
            self.pip_placeholder_texture = Some(pip_placeholder);

            tracing::info!(
                "BlendingCompositor: Initialized ({}x{} default, PiP slide: {}s)",
                self.config.width,
                self.config.height,
                self.config.pip_slide_duration
            );
            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "BlendingCompositor: Shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        let process_start = Instant::now();
        let frame_count = self.frame_count.load(Ordering::Relaxed);

        // Stutter detection
        if let Some(last_time) = self.last_frame_time {
            let interval_ms = last_time.elapsed().as_secs_f64() * 1000.0;
            if interval_ms > 50.0 {
                tracing::warn!(
                    "BlendingCompositor: STUTTER! Frame {} interval: {:.1}ms",
                    frame_count,
                    interval_ms
                );
            }
        }
        self.last_frame_time = Some(process_start);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Lazy texture cache creation
        if self.texture_cache.is_none() {
            self.texture_cache = Some(gpu_ctx.create_texture_cache()?);
        }
        let texture_cache = self.texture_cache.as_ref().unwrap();

        // Update cached textures from new input frames
        if self.inputs.has_data("video_in") {
            let video_frame: Videoframe = self.inputs.read("video_in")?;
            let buffer = gpu_ctx.resolve_videoframe_buffer(&video_frame)?;
            self.cached_video_view = Some(texture_cache.create_view(&buffer)?);
            self.cached_video_dimensions = Some((video_frame.width, video_frame.height));

            // Record when camera first starts flowing
            if self.first_video_time.is_none() {
                self.first_video_time = Some(Instant::now());
                tracing::info!(
                    "BlendingCompositor: First camera frame received, PiP slide-in in {:.1}s",
                    self.config.pip_slide_delay
                );
            }
        }

        if self.inputs.has_data("lower_third_in") {
            let lower_third_frame: Videoframe = self.inputs.read("lower_third_in")?;
            let buffer = gpu_ctx.resolve_videoframe_buffer(&lower_third_frame)?;
            self.cached_lower_third_view = Some(texture_cache.create_view(&buffer)?);
        }

        if self.inputs.has_data("watermark_in") {
            let watermark_frame: Videoframe = self.inputs.read("watermark_in")?;
            let buffer = gpu_ctx.resolve_videoframe_buffer(&watermark_frame)?;
            self.cached_watermark_view = Some(texture_cache.create_view(&buffer)?);
        }

        // Check for PiP frames
        if self.inputs.has_data("pip_in") {
            let pip_frame: Videoframe = self.inputs.read("pip_in")?;
            let buffer = gpu_ctx.resolve_videoframe_buffer(&pip_frame)?;
            self.cached_pip_view = Some(texture_cache.create_view(&buffer)?);
        }

        // Trigger PiP slide-in after delay from first camera frame
        if !self.pip_ready {
            if let Some(first_video) = self.first_video_time {
                if first_video.elapsed().as_secs_f32() >= self.config.pip_slide_delay {
                    self.pip_ready = true;
                    self.pip_animation_start = Some(Instant::now());
                    tracing::info!("BlendingCompositor: PiP slide-in animation starting!");
                }
            }
        }

        // Calculate PiP animation progress
        let pip_slide_progress = if let Some(start_time) = self.pip_animation_start {
            let elapsed = start_time.elapsed().as_secs_f32();
            let progress: f32 = (elapsed / self.config.pip_slide_duration).min(1.0);
            // Ease-out cubic: 1 - (1-t)^3
            1.0_f32 - (1.0_f32 - progress).powi(3)
        } else {
            0.0
        };

        // Determine output dimensions
        let (width, height) = self
            .cached_video_dimensions
            .unwrap_or((self.config.width, self.config.height));

        // Acquire output buffer from GpuContext pool
        let (output_pool_id, output_buffer) =
            gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;
        let output_view = texture_cache.create_view(&output_buffer)?;
        let output_metal: &metal::TextureRef = output_view.as_metal_texture();

        // Update render pass
        let render_pass_desc = self.render_pass_desc.as_ref().unwrap();
        render_pass_desc
            .color_attachments()
            .object_at(0)
            .unwrap()
            .set_texture(Some(output_metal));

        // Render
        let command_queue = gpu_ctx.command_queue().metal_queue_ref();
        let command_buffer = command_queue.new_command_buffer();
        let render_enc = command_buffer.new_render_command_encoder(render_pass_desc);
        render_enc.set_render_pipeline_state(self.render_pipeline.as_ref().unwrap());

        // Bind textures
        let has_video = self.cached_video_view.is_some();
        let has_lower_third = self.cached_lower_third_view.is_some();
        let has_watermark = self.cached_watermark_view.is_some();
        let has_pip = self.pip_ready;

        if let Some(ref video_view) = self.cached_video_view {
            render_enc.set_fragment_texture(0, Some(video_view.as_metal_texture()));
        }
        if let Some(ref lower_third_view) = self.cached_lower_third_view {
            render_enc.set_fragment_texture(1, Some(lower_third_view.as_metal_texture()));
        }
        if let Some(ref watermark_view) = self.cached_watermark_view {
            render_enc.set_fragment_texture(2, Some(watermark_view.as_metal_texture()));
        }
        if let Some(ref pip_view) = self.cached_pip_view {
            render_enc.set_fragment_texture(3, Some(pip_view.as_metal_texture()));
        } else if has_pip {
            if let Some(ref placeholder) = self.pip_placeholder_texture {
                let tex_ref: &metal::TextureRef = placeholder;
                render_enc.set_fragment_texture(3, Some(tex_ref));
            }
        }

        render_enc.set_fragment_sampler_state(0, Some(self.sampler.as_ref().unwrap()));

        // Update uniforms
        self.uniforms_index = (self.uniforms_index + 1) % 3;
        let current_uniforms = self.uniforms_buffers[self.uniforms_index].as_ref().unwrap();

        unsafe {
            let ptr = current_uniforms.contents() as *mut BlendingUniforms;
            (*ptr).has_video = if has_video { 1 } else { 0 };
            (*ptr).has_lower_third = if has_lower_third { 1 } else { 0 };
            (*ptr).has_watermark = if has_watermark { 1 } else { 0 };
            (*ptr).has_pip = if has_pip { 1 } else { 0 };
            (*ptr).pip_slide_progress = pip_slide_progress;
            (*ptr)._padding1 = 0.0;
            (*ptr)._padding2 = 0.0;
            (*ptr)._padding3 = 0.0;
        }

        render_enc.set_fragment_buffer(0, Some(current_uniforms), 0);

        render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_enc.end_encoding();

        command_buffer.commit();
        command_buffer.wait_until_completed();

        // Output frame
        let timestamp_ns = (frame_count as i64) * 16_666_667;
        let output_frame = Videoframe {
            surface_id: output_pool_id.to_string(),
            width,
            height,
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: frame_count.to_string(),
        };
        self.outputs.write("video_out", &output_frame)?;

        self.frame_count.fetch_add(1, Ordering::Relaxed);

        let total_ms = process_start.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 20.0 {
            tracing::warn!(
                "BlendingCompositor: Frame {} slow: {:.1}ms",
                frame_count,
                total_ms
            );
        }

        Ok(())
    }
}
