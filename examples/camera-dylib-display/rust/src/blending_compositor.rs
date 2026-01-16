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
use streamlib::core::rhi::{
    PixelBufferDescriptor, PixelFormat, RhiPixelBufferPool, RhiTextureCache, RhiTextureView,
};
use streamlib::core::{
    GpuContext, LinkInput, LinkOutput, Result, RuntimeContext, StreamError, VideoFrame,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct BlendingCompositorConfig {
    /// Default output width (used until video arrives)
    pub width: u32,
    /// Default output height (used until video arrives)
    pub height: u32,
    /// Duration of PiP slide-in animation in seconds
    pub pip_slide_duration: f32,
}

impl Default for BlendingCompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            pip_slide_duration: 0.5, // 500ms slide-in
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

#[streamlib::processor(
    name = "BlendingCompositor",
    execution = Reactive,
    description = "Multi-layer alpha blending compositor with PiP support",
    unsafe_send
)]
pub struct BlendingCompositorProcessor {
    #[streamlib::input(description = "Video frames (base layer)")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::input(description = "Lower third overlay (RGBA with transparency)")]
    lower_third_in: LinkInput<VideoFrame>,

    #[streamlib::input(description = "Watermark overlay (RGBA with transparency)")]
    watermark_in: LinkInput<VideoFrame>,

    #[streamlib::input(description = "PiP overlay (avatar character with transparent background)")]
    pip_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Composited video frames")]
    video_out: LinkOutput<VideoFrame>,

    #[streamlib::config]
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
    output_buffers: [Option<streamlib::core::rhi::RhiPixelBuffer>; 3],
    output_index: usize,
    output_dimensions: Option<(u32, u32)>,

    // Cached texture views
    cached_video_view: Option<RhiTextureView>,
    cached_lower_third_view: Option<RhiTextureView>,
    cached_watermark_view: Option<RhiTextureView>,
    cached_pip_view: Option<RhiTextureView>,
    cached_video_dimensions: Option<(u32, u32)>,

    // PiP animation state
    pip_ready: bool,
    pip_animation_start: Option<Instant>,

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

            self.render_pipeline = Some(render_pipeline);
            self.render_pass_desc = Some(render_pass_desc_ref.to_owned());
            self.sampler = Some(sampler);
            self.uniforms_buffers = uniforms_buffers;
            self.uniforms_index = 0;
            self.pip_ready = false;
            self.pip_animation_start = None;

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
        if let Some(video_frame) = self.video_in.read() {
            self.cached_video_view = Some(texture_cache.create_view(video_frame.buffer())?);
            self.cached_video_dimensions = Some((video_frame.width(), video_frame.height()));
        }

        if let Some(lower_third_frame) = self.lower_third_in.read() {
            self.cached_lower_third_view =
                Some(texture_cache.create_view(lower_third_frame.buffer())?);
        }

        if let Some(watermark_frame) = self.watermark_in.read() {
            self.cached_watermark_view = Some(texture_cache.create_view(watermark_frame.buffer())?);
        }

        // Check for PiP frames and "pip_ready" signal
        if let Some(pip_frame) = self.pip_in.read() {
            self.cached_pip_view = Some(texture_cache.create_view(pip_frame.buffer())?);

            // Check if avatar processor signaled ready (first pose detected)
            // The pip_ready flag is passed as metadata in the frame
            // For now, we trigger animation on first PiP frame with content
            if !self.pip_ready {
                // Start animation when first PiP frame arrives
                self.pip_ready = true;
                self.pip_animation_start = Some(Instant::now());
                tracing::info!("BlendingCompositor: PiP ready - starting slide-in animation!");
            }
        }

        // Calculate PiP animation progress
        let pip_slide_progress = if let Some(start_time) = self.pip_animation_start {
            let elapsed = start_time.elapsed().as_secs_f32();
            let progress = (elapsed / self.config.pip_slide_duration).min(1.0);
            // Ease-out cubic: 1 - (1-t)^3
            1.0 - (1.0 - progress).powi(3)
        } else {
            0.0
        };

        // Determine output dimensions
        let (width, height) = self
            .cached_video_dimensions
            .unwrap_or((self.config.width, self.config.height));

        // Output buffer ring
        if self.output_dimensions != Some((width, height)) {
            let output_desc = PixelBufferDescriptor::new(width, height, PixelFormat::Bgra32);
            let pool = RhiPixelBufferPool::new_with_descriptor(&output_desc)?;
            self.output_buffers = [
                Some(pool.acquire()?),
                Some(pool.acquire()?),
                Some(pool.acquire()?),
            ];
            self.output_dimensions = Some((width, height));
            self.output_index = 0;
        }

        self.output_index = (self.output_index + 1) % 3;
        let output_buffer = self.output_buffers[self.output_index]
            .as_ref()
            .unwrap()
            .clone();
        let output_view = texture_cache.create_view(&output_buffer)?;
        let output_metal = output_view.as_metal_texture();

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
        let has_pip = self.cached_pip_view.is_some() && self.pip_ready;

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

        // Output frame
        let timestamp_ns = (frame_count as i64) * 16_666_667;
        let output_frame = VideoFrame::from_buffer(output_buffer, timestamp_ns, frame_count);
        self.video_out.write(output_frame);

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
