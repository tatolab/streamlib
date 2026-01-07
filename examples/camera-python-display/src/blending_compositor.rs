// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Blending Compositor - Multi-layer alpha blending for parallel pipelines.
//!
//! Composites multiple video layers using Photoshop-style alpha blending:
//! - Layer 1 (base): Video from camera/segmentation pipeline
//! - Layer 2 (middle): Lower third overlay (RGBA with transparency)
//! - Layer 3 (top): Watermark overlay (RGBA with transparency)
//!
//! Continuous execution at 60fps (16ms interval). All inputs are cached
//! and reused when not updated, decoupling output rate from input rates.

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
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
}

impl Default for BlendingCompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}

#[streamlib::processor(
    name = "BlendingCompositor",
    execution = Reactive,
    description = "Multi-layer alpha blending compositor for parallel pipelines",
    unsafe_send
)]
pub struct BlendingCompositorProcessor {
    #[streamlib::input(description = "Video frames (base layer)")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::input(description = "Lower third overlay (RGBA with transparency)")]
    lower_third_in: LinkInput<VideoFrame>,

    #[streamlib::input(description = "Watermark overlay (RGBA with transparency)")]
    watermark_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Composited video frames")]
    video_out: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: BlendingCompositorConfig,

    gpu_context: Option<GpuContext>,
    render_pipeline: Option<metal::RenderPipelineState>,
    render_pass_desc: Option<metal::RenderPassDescriptor>,
    sampler: Option<metal::SamplerState>,
    frame_count: AtomicU64,

    // RHI resources - one texture cache handles all views
    texture_cache: Option<RhiTextureCache>,
    output_pool: Option<RhiPixelBufferPool>,
    output_pool_dimensions: Option<(u32, u32)>,

    // Cached texture views (reused when no new frame arrives)
    // We store the RhiTextureView which keeps the texture valid
    cached_video_view: Option<RhiTextureView>,
    cached_lower_third_view: Option<RhiTextureView>,
    cached_watermark_view: Option<RhiTextureView>,
    cached_video_dimensions: Option<(u32, u32)>,

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
            self.texture_cache = None; // Deferred to first process() to avoid race

            // Get Metal device ref for shader compilation
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

            // Render pipeline with alpha blending enabled
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

            // Create render pass descriptor (reused each frame, only texture attachment updated)
            let render_pass_desc_ref = metal::RenderPassDescriptor::new();
            let color_attachment = render_pass_desc_ref
                .color_attachments()
                .object_at(0)
                .unwrap();
            color_attachment.set_load_action(metal::MTLLoadAction::Clear);
            color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);

            self.render_pipeline = Some(render_pipeline);
            self.render_pass_desc = Some(render_pass_desc_ref.to_owned());
            self.sampler = Some(sampler);

            tracing::info!(
                "BlendingCompositor: Initialized ({}x{} default)",
                self.config.width,
                self.config.height
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

        // Check frame interval for stutters (camera is 30fps = ~33ms, threshold at 50ms)
        if let Some(last_time) = self.last_frame_time {
            let interval_ms = last_time.elapsed().as_secs_f64() * 1000.0;
            if interval_ms > 50.0 {
                tracing::warn!(
                    "BlendingCompositor: STUTTER detected! Frame {} interval: {:.1}ms (expected ~33ms for 30fps)",
                    frame_count, interval_ms
                );
            }
        }
        self.last_frame_time = Some(process_start);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Lazy texture cache creation (deferred from setup)
        if self.texture_cache.is_none() {
            self.texture_cache = Some(gpu_ctx.create_texture_cache()?);
        }
        let texture_cache = self.texture_cache.as_ref().unwrap();

        // Update cached textures from any new input frames
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

        // Use video dimensions if available, otherwise config defaults
        let (width, height) = self
            .cached_video_dimensions
            .unwrap_or((self.config.width, self.config.height));

        // Acquire output buffer (recreate pool if dimensions changed)
        if self.output_pool.is_none() || self.output_pool_dimensions != Some((width, height)) {
            let output_desc = PixelBufferDescriptor::new(width, height, PixelFormat::Bgra32);
            self.output_pool = Some(RhiPixelBufferPool::new_with_descriptor(&output_desc)?);
            self.output_pool_dimensions = Some((width, height));
        }
        let output_buffer = self.output_pool.as_ref().unwrap().acquire()?;
        let output_view = texture_cache.create_view(&output_buffer)?;
        let output_metal = output_view.as_metal_texture();

        // Update render pass attachment with current output texture (descriptor cached from setup)
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

        // Bind textures - use cached views if available, shader handles missing layers
        let has_video = self.cached_video_view.is_some();
        let has_lower_third = self.cached_lower_third_view.is_some();
        let has_watermark = self.cached_watermark_view.is_some();

        if let Some(ref video_view) = self.cached_video_view {
            render_enc.set_fragment_texture(0, Some(video_view.as_metal_texture()));
        }
        if let Some(ref lower_third_view) = self.cached_lower_third_view {
            render_enc.set_fragment_texture(1, Some(lower_third_view.as_metal_texture()));
        }
        if let Some(ref watermark_view) = self.cached_watermark_view {
            render_enc.set_fragment_texture(2, Some(watermark_view.as_metal_texture()));
        }

        render_enc.set_fragment_sampler_state(0, Some(self.sampler.as_ref().unwrap()));

        // Pass layer availability flags to shader
        let has_video_flag: u32 = if has_video { 1 } else { 0 };
        let has_lower_third_flag: u32 = if has_lower_third { 1 } else { 0 };
        let has_watermark_flag: u32 = if has_watermark { 1 } else { 0 };

        render_enc.set_fragment_bytes(
            0,
            std::mem::size_of::<u32>() as u64,
            &has_video_flag as *const u32 as *const c_void,
        );
        render_enc.set_fragment_bytes(
            1,
            std::mem::size_of::<u32>() as u64,
            &has_lower_third_flag as *const u32 as *const c_void,
        );
        render_enc.set_fragment_bytes(
            2,
            std::mem::size_of::<u32>() as u64,
            &has_watermark_flag as *const u32 as *const c_void,
        );

        render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_enc.end_encoding();

        command_buffer.commit();
        let gpu_wait_start = Instant::now();
        command_buffer.wait_until_completed();
        let gpu_wait_ms = gpu_wait_start.elapsed().as_secs_f64() * 1000.0;

        // Always output a frame
        let timestamp_ns = (frame_count as i64) * 16_666_667; // ~60fps in ns
        let output_frame = VideoFrame::from_buffer(output_buffer, timestamp_ns, frame_count);
        self.video_out.write(output_frame);

        self.frame_count.fetch_add(1, Ordering::Relaxed);

        let total_ms = process_start.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 20.0 || gpu_wait_ms > 10.0 {
            tracing::warn!(
                "BlendingCompositor: Frame {} slow: total={:.1}ms, gpu_wait={:.1}ms",
                frame_count,
                total_ms,
                gpu_wait_ms
            );
        }

        Ok(())
    }
}
