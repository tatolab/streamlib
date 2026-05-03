// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Blending Compositor — multi-layer alpha-over composite with PiP slide-in.
//!
//! Runs as a [`ManualProcessor`] with a render thread paced against the
//! display's refresh rate (60 Hz fallback). Each tick reads the latest
//! frame from each input port (older queued frames are dropped by the
//! port's `SkipToLatest` read mode), composites the four layers, and
//! emits one output frame.
//!
//! Layer order (bottom → top): video → lower_third → watermark → PiP.
//! macOS uses a Metal fragment shader; Linux uses
//! [`streamlib::vulkan::rhi::VulkanBlendingCompositor`].

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use streamlib::core::display_info;
use streamlib::core::rhi::PixelFormat;
use streamlib::core::{GpuContextLimitedAccess, Result, RuntimeContextFullAccess, StreamError};
use streamlib::iceoryx2::{InputMailboxes, OutputWriter};
use streamlib::Videoframe;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::core::rhi::{RhiTextureCache, RhiTextureView};

#[cfg(target_os = "linux")]
use std::sync::Arc as StdArc;
#[cfg(target_os = "linux")]
use streamlib::{BlendingCompositorInputs, VulkanBlendingCompositor};

// Per-platform GPU backend stash. Defined as a single field on the
// processor (proc-macro `#[streamlib::processor]` strips `#[cfg]` attrs
// from individual fields, so we collapse the cfg into the type alias).
#[cfg(any(target_os = "macos", target_os = "ios"))]
type GpuBackendStash = Option<MetalState>;
#[cfg(target_os = "linux")]
type GpuBackendStash = Option<StdArc<VulkanBlendingCompositor>>;
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
type GpuBackendStash = ();

/// Iteration cap applied when [`BlendingCompositorConfig::target_fps`]
/// or the display refresh query produces a non-positive value.
const FALLBACK_TARGET_FPS: f64 = 60.0;

/// Render-loop iteration count slack: at 60 Hz nominal we tolerate up
/// to ~5 fps over (drift between sleep granularity + scheduler) before
/// the loop is considered "out of bound" — issue exit-criterion
/// "60 → ≤ 65/s on a 60 Hz display".
#[cfg_attr(not(test), allow(dead_code))]
const TARGET_FPS_OVERSHOOT_SLACK: f64 = 5.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlendingCompositorConfig {
    /// Default output width (used until video arrives).
    pub width: u32,
    /// Default output height (used until video arrives).
    pub height: u32,
    /// Duration of PiP slide-in animation, seconds.
    pub pip_slide_duration: f32,
    /// Delay after first camera frame before PiP slides in, seconds.
    pub pip_slide_delay: f32,
    /// Override the render loop's target frame rate. When unset, the
    /// loop polls [`display_info::get_refresh_rate`]; primarily useful
    /// for tests that need a deterministic cadence without a window.
    #[serde(default)]
    pub target_fps: Option<f64>,
}

impl Default for BlendingCompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            pip_slide_duration: 0.5,
            pip_slide_delay: 2.5,
            target_fps: None,
        }
    }
}

#[streamlib::processor("com.tatolab.blending_compositor")]
pub struct BlendingCompositorProcessor {
    config: BlendingCompositorConfig,

    gpu_context: Option<GpuContextLimitedAccess>,
    frame_count: Arc<AtomicU64>,

    /// Stop signal for the render thread.
    running: Arc<AtomicBool>,
    /// Render-thread handle owned by this processor; joined on `stop()`.
    render_thread: Option<JoinHandle<()>>,

    /// Platform-specific GPU backend instantiated in `setup()` and
    /// moved into the render thread by `start()`.
    backend: GpuBackendStash,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
struct MetalState {
    render_pipeline: metal::RenderPipelineState,
    sampler: metal::SamplerState,
    render_pass_desc: metal::RenderPassDescriptor,
    uniforms_buffers: [metal::Buffer; 3],
    pip_placeholder_texture: metal::Texture,
}

/// Uniform buffer for the Metal compositor shader. Must match the Metal
/// struct layout exactly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[repr(C)]
struct BlendingUniforms {
    has_video: u32,
    has_lower_third: u32,
    has_watermark: u32,
    has_pip: u32,
    pip_slide_progress: f32,
    _padding1: f32,
    _padding2: f32,
    _padding3: f32,
}

impl streamlib::core::ManualProcessor for BlendingCompositorProcessor::Processor {
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
            "BlendingCompositor: teardown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let target_fps = self.resolve_target_fps();
        tracing::info!(
            "BlendingCompositor: starting render thread @ {:.1} fps",
            target_fps
        );

        let inputs = std::mem::take(&mut self.inputs);
        // Outputs are an `Arc<OutputWriter>` in the generated processor —
        // share the writer with the render thread by cloning the handle.
        let outputs = Arc::clone(&self.outputs);
        let running = Arc::clone(&self.running);
        let frame_count = Arc::clone(&self.frame_count);
        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let config = self.config.clone();

        running.store(true, Ordering::Release);

        let backend = self
            .backend
            .take()
            .ok_or_else(|| StreamError::Configuration("GPU backend not initialized".into()))?;

        let thread = std::thread::Builder::new()
            .name("blending-compositor-render".into())
            .spawn(move || {
                let mut state = LoopState::new(config);
                manual_render_loop(target_fps, Arc::clone(&running), || {
                    let _ = compose_one_frame(
                        &mut state,
                        &gpu_context,
                        &inputs,
                        &outputs,
                        &frame_count,
                        &backend,
                    );
                });
            })
            .map_err(|e| StreamError::Runtime(format!("Failed to spawn render thread: {e}")))?;

        self.render_thread = Some(thread);
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.render_thread.take() {
            handle
                .join()
                .map_err(|_| StreamError::Runtime("Render thread panicked".into()))?;
        }
        tracing::info!(
            "BlendingCompositor: stopped ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

impl BlendingCompositorProcessor::Processor {
    fn resolve_target_fps(&self) -> f64 {
        if let Some(t) = self.config.target_fps {
            return if t > 0.0 { t } else { FALLBACK_TARGET_FPS };
        }
        // No window in BlendingCompositor — Linux falls back to 60 Hz; macOS
        // queries the main display via CoreGraphics, no window needed.
        let rate = display_info::get_refresh_rate(None);
        if rate > 0.0 { rate } else { FALLBACK_TARGET_FPS }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("BlendingCompositor: setup (Metal)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        let metal_device_ref = ctx.gpu_full_access().device().metal_device_ref();

        let shader_source = include_str!("shaders/blending_compositor.metal");
        let library = metal_device_ref
            .new_library_with_source(shader_source, &metal::CompileOptions::new())
            .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {e}")))?;

        let vertex_fn = library
            .get_function("blending_vertex", None)
            .map_err(|e| StreamError::Configuration(format!("Vertex not found: {e}")))?;
        let fragment_fn = library
            .get_function("blending_fragment", None)
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
        let attachment = render_pass_desc_ref.color_attachments().object_at(0).unwrap();
        attachment.set_load_action(metal::MTLLoadAction::Clear);
        attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
        attachment.set_store_action(metal::MTLStoreAction::Store);

        let uniforms_size = std::mem::size_of::<BlendingUniforms>() as u64;
        let make_uniforms = || {
            metal_device_ref.new_buffer(uniforms_size, metal::MTLResourceOptions::CPUCacheModeDefaultCache)
        };
        let uniforms_buffers = [make_uniforms(), make_uniforms(), make_uniforms()];

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

        self.backend = Some(MetalState {
            render_pipeline,
            sampler,
            render_pass_desc: render_pass_desc_ref.to_owned(),
            uniforms_buffers,
            pip_placeholder_texture: pip_placeholder,
        });

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("BlendingCompositor: setup (Vulkan)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        let vulkan_device = ctx.gpu_full_access().device().vulkan_device().clone();
        let compositor = VulkanBlendingCompositor::new(&vulkan_device)?;
        self.backend = Some(StdArc::new(compositor));
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    fn setup_inner(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let _ = ctx;
        Err(StreamError::Configuration(
            "BlendingCompositor: no GPU backend on this platform".into(),
        ))
    }
}

// ---- Render-loop scaffolding ------------------------------------------------

/// Per-iteration state owned by the spawned render thread. Tracks PiP
/// animation timing and (on macOS) the cached input texture views.
struct LoopState {
    config: BlendingCompositorConfig,
    pip_ready: bool,
    pip_animation_start: Option<Instant>,
    first_video_time: Option<Instant>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    texture_cache: Option<RhiTextureCache>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    cached_video_view: Option<RhiTextureView>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    cached_lower_third_view: Option<RhiTextureView>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    cached_watermark_view: Option<RhiTextureView>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    cached_pip_view: Option<RhiTextureView>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    uniforms_index: usize,
    cached_video_dimensions: Option<(u32, u32)>,
}

impl LoopState {
    fn new(config: BlendingCompositorConfig) -> Self {
        Self {
            config,
            pip_ready: false,
            pip_animation_start: None,
            first_video_time: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            texture_cache: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            cached_video_view: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            cached_lower_third_view: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            cached_watermark_view: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            cached_pip_view: None,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            uniforms_index: 0,
            cached_video_dimensions: None,
        }
    }

    fn maybe_promote_pip(&mut self, now: Instant) {
        if self.pip_ready {
            return;
        }
        if let Some(start) = self.first_video_time {
            if start.elapsed().as_secs_f32() >= self.config.pip_slide_delay {
                self.pip_ready = true;
                self.pip_animation_start = Some(now);
                tracing::info!("BlendingCompositor: PiP slide-in started");
            }
        }
    }

    fn pip_slide_progress(&self) -> f32 {
        let Some(start) = self.pip_animation_start else {
            return 0.0;
        };
        let elapsed = start.elapsed().as_secs_f32();
        let progress = (elapsed / self.config.pip_slide_duration).min(1.0);
        // Ease-out cubic — preserved verbatim from the Metal Reactive impl.
        1.0_f32 - (1.0_f32 - progress).powi(3)
    }
}

/// Slow-tick threshold restored from the pre-rewrite `STUTTER!` log
/// (50 ms ≈ three frames at 60 Hz). Exceeding this is a clear hitch
/// worth surfacing even when the loop's per-tick cadence still
/// averages out.
const SLOW_TICK_WARN_THRESHOLD: Duration = Duration::from_millis(50);

/// Render loop. Sleeps after each tick to maintain `target_fps` cadence;
/// exits when `running` is cleared. The closure runs once per iteration.
///
/// When a tick spends longer than `frame_period`, the deadline baseline
/// is reset to `now` so the loop does not spiral trying to "catch up"
/// — but each over-budget tick is also surfaced via [`tracing::warn!`]
/// (gated by [`SLOW_TICK_WARN_THRESHOLD`]) so sustained slowness stays
/// visible rather than silently masked.
fn manual_render_loop<F>(target_fps: f64, running: Arc<AtomicBool>, mut tick: F)
where
    F: FnMut(),
{
    let frame_period = if target_fps > 0.0 {
        Duration::from_secs_f64(1.0 / target_fps)
    } else {
        Duration::from_millis(16)
    };
    let mut next_deadline = Instant::now();
    while running.load(Ordering::Acquire) {
        let tick_start = Instant::now();
        tick();
        let tick_elapsed = tick_start.elapsed();
        if tick_elapsed > SLOW_TICK_WARN_THRESHOLD {
            tracing::warn!(
                "BlendingCompositor: slow tick {:?} (threshold {:?})",
                tick_elapsed,
                SLOW_TICK_WARN_THRESHOLD
            );
        }
        next_deadline += frame_period;
        let now = Instant::now();
        if next_deadline > now {
            std::thread::sleep(next_deadline - now);
        } else {
            // Falling behind — reset baseline so we don't spiral when a
            // tick spends longer than `frame_period`. The slow-tick
            // tracing above keeps the issue visible despite the reset.
            next_deadline = now;
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(clippy::too_many_arguments)]
fn compose_one_frame(
    state: &mut LoopState,
    gpu_ctx: &GpuContextLimitedAccess,
    inputs: &InputMailboxes,
    outputs: &Arc<OutputWriter>,
    frame_count: &Arc<AtomicU64>,
    backend: &MetalState,
) -> Result<()> {
    if state.texture_cache.is_none() {
        state.texture_cache = Some(gpu_ctx.create_texture_cache()?);
    }
    let texture_cache = state.texture_cache.as_ref().unwrap();

    if inputs.has_data("video_in") {
        let frame: Videoframe = inputs.read("video_in")?;
        let buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        state.cached_video_view = Some(texture_cache.create_view(&buffer)?);
        state.cached_video_dimensions = Some((frame.width, frame.height));
        if state.first_video_time.is_none() {
            state.first_video_time = Some(Instant::now());
        }
    }
    if inputs.has_data("lower_third_in") {
        let frame: Videoframe = inputs.read("lower_third_in")?;
        let buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        state.cached_lower_third_view = Some(texture_cache.create_view(&buffer)?);
    }
    if inputs.has_data("watermark_in") {
        let frame: Videoframe = inputs.read("watermark_in")?;
        let buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        state.cached_watermark_view = Some(texture_cache.create_view(&buffer)?);
    }
    if inputs.has_data("pip_in") {
        let frame: Videoframe = inputs.read("pip_in")?;
        let buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        state.cached_pip_view = Some(texture_cache.create_view(&buffer)?);
    }

    state.maybe_promote_pip(Instant::now());
    let pip_slide_progress = state.pip_slide_progress();

    let (width, height) = state
        .cached_video_dimensions
        .unwrap_or((state.config.width, state.config.height));

    let (output_pool_id, output_buffer) =
        gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;
    let output_view = texture_cache.create_view(&output_buffer)?;
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

    let has_video = state.cached_video_view.is_some();
    let has_lower_third = state.cached_lower_third_view.is_some();
    let has_watermark = state.cached_watermark_view.is_some();
    let has_pip = state.pip_ready;

    if let Some(ref view) = state.cached_video_view {
        render_enc.set_fragment_texture(0, Some(view.as_metal_texture()));
    }
    if let Some(ref view) = state.cached_lower_third_view {
        render_enc.set_fragment_texture(1, Some(view.as_metal_texture()));
    }
    if let Some(ref view) = state.cached_watermark_view {
        render_enc.set_fragment_texture(2, Some(view.as_metal_texture()));
    }
    if let Some(ref view) = state.cached_pip_view {
        render_enc.set_fragment_texture(3, Some(view.as_metal_texture()));
    } else if has_pip {
        let tex_ref: &metal::TextureRef = &backend.pip_placeholder_texture;
        render_enc.set_fragment_texture(3, Some(tex_ref));
    }

    render_enc.set_fragment_sampler_state(0, Some(&backend.sampler));

    state.uniforms_index = (state.uniforms_index + 1) % backend.uniforms_buffers.len();
    let uniforms = &backend.uniforms_buffers[state.uniforms_index];
    unsafe {
        let ptr = uniforms.contents() as *mut BlendingUniforms;
        (*ptr).has_video = has_video as u32;
        (*ptr).has_lower_third = has_lower_third as u32;
        (*ptr).has_watermark = has_watermark as u32;
        (*ptr).has_pip = has_pip as u32;
        (*ptr).pip_slide_progress = pip_slide_progress;
        (*ptr)._padding1 = 0.0;
        (*ptr)._padding2 = 0.0;
        (*ptr)._padding3 = 0.0;
    }
    render_enc.set_fragment_buffer(0, Some(uniforms), 0);
    render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
    render_enc.end_encoding();
    command_buffer.commit();
    command_buffer.wait_until_completed();

    let count = frame_count.fetch_add(1, Ordering::Relaxed);
    let timestamp_ns = (count as i64) * 16_666_667;
    let output_frame = Videoframe {
        surface_id: output_pool_id.to_string(),
        width,
        height,
        timestamp_ns: timestamp_ns.to_string(),
        frame_index: count.to_string(),
        fps: None,
        // Per-frame override is opt-in (#633); per-surface
        // `current_image_layout` from surface-share is the default.
        texture_layout: None,
    };
    outputs.write("video_out", &output_frame)?;

    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn compose_one_frame(
    state: &mut LoopState,
    gpu_ctx: &GpuContextLimitedAccess,
    inputs: &InputMailboxes,
    outputs: &Arc<OutputWriter>,
    frame_count: &Arc<AtomicU64>,
    backend: &StdArc<VulkanBlendingCompositor>,
) -> Result<()> {
    // The loop runs unconditionally at refresh rate (per the issue's
    // "idle invocation count is bounded by display refresh + small
    // slack" exit criterion). When no upstream frame has ever arrived
    // the kernel still dispatches and emits a dark-blue placeholder
    // — same as the macOS Metal path. The cost (one GPU dispatch on
    // 1×1 placeholder buffers) is negligible and keeps the pipeline
    // cadence steady; downstream consumers see a stream of valid
    // frames from t=0 instead of a stall before the first input.
    let video_buf = if inputs.has_data("video_in") {
        let frame: Videoframe = inputs.read("video_in")?;
        let buf = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        state.cached_video_dimensions = Some((frame.width, frame.height));
        if state.first_video_time.is_none() {
            state.first_video_time = Some(Instant::now());
        }
        Some(buf)
    } else {
        None
    };
    let lower_third_buf = inputs
        .has_data("lower_third_in")
        .then(|| inputs.read::<Videoframe>("lower_third_in"))
        .transpose()?
        .map(|f| gpu_ctx.resolve_videoframe_buffer(&f))
        .transpose()?;
    let watermark_buf = inputs
        .has_data("watermark_in")
        .then(|| inputs.read::<Videoframe>("watermark_in"))
        .transpose()?
        .map(|f| gpu_ctx.resolve_videoframe_buffer(&f))
        .transpose()?;
    let pip_buf = inputs
        .has_data("pip_in")
        .then(|| inputs.read::<Videoframe>("pip_in"))
        .transpose()?
        .map(|f| gpu_ctx.resolve_videoframe_buffer(&f))
        .transpose()?;

    state.maybe_promote_pip(Instant::now());
    let pip_slide_progress = state.pip_slide_progress();

    let (width, height) = state
        .cached_video_dimensions
        .unwrap_or((state.config.width, state.config.height));

    let (output_pool_id, output_buffer) =
        gpu_ctx.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;

    backend.dispatch(BlendingCompositorInputs {
        video: video_buf.as_ref(),
        lower_third: lower_third_buf.as_ref(),
        watermark: watermark_buf.as_ref(),
        pip: if state.pip_ready { pip_buf.as_ref() } else { None },
        output: &output_buffer,
        pip_slide_progress,
    })?;

    let count = frame_count.fetch_add(1, Ordering::Relaxed);
    let timestamp_ns = (count as i64) * 16_666_667;
    let output_frame = Videoframe {
        surface_id: output_pool_id.to_string(),
        width,
        height,
        timestamp_ns: timestamp_ns.to_string(),
        frame_index: count.to_string(),
        fps: None,
        // Per-frame override is opt-in (#633); per-surface
        // `current_image_layout` from surface-share is the default.
        texture_layout: None,
    };
    outputs.write("video_out", &output_frame)?;

    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn compose_one_frame(
    _state: &mut LoopState,
    _gpu_ctx: &GpuContextLimitedAccess,
    _inputs: &InputMailboxes,
    _outputs: &Arc<OutputWriter>,
    _frame_count: &Arc<AtomicU64>,
    _backend: &(),
) -> Result<()> {
    Err(StreamError::Configuration(
        "BlendingCompositor: no GPU backend on this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Issue exit-criterion: idle invocation count is bounded by display
    /// refresh + small slack (60 → ≤ 65/s on a 60 Hz display).
    #[test]
    fn manual_loop_runs_at_target_fps() {
        let target_fps: f64 = 60.0;
        let running = Arc::new(AtomicBool::new(true));
        let counter = Arc::new(AtomicU64::new(0));

        let counter_clone = Arc::clone(&counter);
        let running_clone = Arc::clone(&running);
        let handle = std::thread::spawn(move || {
            manual_render_loop(target_fps, running_clone, || {
                counter_clone.fetch_add(1, Ordering::Relaxed);
            });
        });

        std::thread::sleep(Duration::from_millis(2000));
        running.store(false, Ordering::Release);
        handle.join().expect("loop thread join");

        let observed = counter.load(Ordering::Relaxed) as f64;
        let nominal = target_fps * 2.0; // 2 seconds at 60 fps = 120
        let lower = nominal - TARGET_FPS_OVERSHOOT_SLACK * 2.0; // tolerate ±5/s
        let upper = nominal + TARGET_FPS_OVERSHOOT_SLACK * 2.0;
        assert!(
            observed >= lower && observed <= upper,
            "expected {nominal} ±{} ticks, got {observed}",
            TARGET_FPS_OVERSHOOT_SLACK * 2.0
        );
    }

    /// Issue exit-criterion: stop signal exits the render loop within 250 ms.
    /// Mirrors the `Arc<AtomicBool>` + explicit-stop pattern used by
    /// `AppleDisplayProcessor` / `LinuxDisplayProcessor`. The original
    /// issue body framed this as a PUBSUB shutdown event; the actual
    /// codebase converged on the bool-based pattern. See in-issue
    /// annotation dated 2026-05-01.
    #[test]
    fn shutdown_exits_loop() {
        let running = Arc::new(AtomicBool::new(true));
        let started = Arc::new(AtomicBool::new(false));

        let running_clone = Arc::clone(&running);
        let started_clone = Arc::clone(&started);
        let handle = std::thread::spawn(move || {
            manual_render_loop(60.0, running_clone, || {
                started_clone.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(8));
            });
        });

        // Wait for the loop to start before signalling stop, so we
        // measure shutdown latency rather than thread-spawn overhead.
        let spawn_deadline = Instant::now() + Duration::from_millis(500);
        while !started.load(Ordering::Acquire) && Instant::now() < spawn_deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(started.load(Ordering::Acquire), "loop never started");

        let stop_at = Instant::now();
        running.store(false, Ordering::Release);
        handle.join().expect("loop thread join");
        let elapsed = stop_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(250),
            "loop took {elapsed:?} to exit after stop signal (cap is 250 ms)"
        );
    }

    /// Locks down the contract the render loop's drain-latest behavior
    /// inherits from the iceoryx2 `SkipToLatest` read mode (the schema
    /// default for video input ports — see
    /// `libs/streamlib-macros/src/codegen.rs` `read_mode_tokens`).
    /// `inputs.read()` calls into `PortMailbox::pop_latest`; if a future
    /// refactor changed this primitive to FIFO behavior, the loop
    /// would silently drift to consuming stale frames.
    ///
    /// **Scope note.** This test exercises the iceoryx2 primitive
    /// directly, not the processor's call into it. A processor-level
    /// integration test would require constructing valid
    /// `FrameHeader`-prefixed wire bytes and pushing them through an
    /// `InputMailboxes` / iceoryx2 subscriber — neither has a public
    /// constructor accessible to an out-of-tree test. The combination
    /// of (a) this primitive test, (b) the schema YAML omitting a
    /// `read_mode` override (so codegen picks `SkipToLatest`), and
    /// (c) the loop calling `inputs.read()` once per tick is what
    /// satisfies the issue's exit criterion.
    #[test]
    fn iceoryx2_pop_latest_skips_stale_frames() {
        use streamlib::iceoryx2::PortMailbox;

        let mailbox = PortMailbox::new(8);
        for i in 0u8..5 {
            mailbox.push(vec![i]);
        }
        let latest = mailbox
            .pop_latest()
            .expect("at least one frame should have been pushed");
        assert_eq!(latest, vec![4], "pop_latest must return the most recent push");
        assert!(
            mailbox.is_empty(),
            "older frames must be drained (skip-stale semantics)"
        );
    }

    /// Verifies the render loop's call shape: each tick pulls **one**
    /// payload from the input source and ignores any that arrived
    /// between ticks. Uses an in-memory queue mock (`Mutex<Vec<u32>>`)
    /// in place of `InputMailboxes::read()` so the test exercises the
    /// loop's per-tick consume model rather than the iceoryx2 primitive.
    #[test]
    fn render_loop_consumes_one_payload_per_tick() {
        use std::sync::Mutex;

        let target_fps: f64 = 60.0;
        let running = Arc::new(AtomicBool::new(true));
        // Pre-seed five "stale" payloads + a marker for the test thread
        // to push the latest one between iterations.
        let queue: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(vec![10, 11, 12, 13, 14]));
        let consumed: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));

        let queue_for_loop = Arc::clone(&queue);
        let consumed_clone = Arc::clone(&consumed);
        let running_clone = Arc::clone(&running);

        let handle = std::thread::spawn(move || {
            manual_render_loop(target_fps, running_clone, || {
                // Mirror `inputs.read()` with `SkipToLatest`: drain any
                // stale entries and consume only the most recent one.
                let mut q = queue_for_loop.lock().unwrap();
                if let Some(latest) = q.pop() {
                    q.clear();
                    drop(q);
                    consumed_clone.lock().unwrap().push(latest);
                }
            });
        });

        // First tick should consume only the latest of the seeded five.
        std::thread::sleep(Duration::from_millis(40));
        // Push one more between iterations; the next tick should pick
        // it up directly with no buffering of the prior consumed frame.
        queue.lock().unwrap().push(99);
        std::thread::sleep(Duration::from_millis(40));

        running.store(false, Ordering::Release);
        handle.join().expect("loop join");

        let observed = consumed.lock().unwrap().clone();
        assert!(
            observed.contains(&14),
            "loop must consume the latest pre-seeded frame, got {observed:?}"
        );
        assert!(
            observed.contains(&99),
            "loop must pick up the post-seed frame, got {observed:?}"
        );
        assert!(
            !observed.contains(&10) && !observed.contains(&11) && !observed.contains(&12),
            "stale frames must NOT be consumed, got {observed:?}"
        );
    }

    /// Sanity check the easing curve so the macOS Metal port and the
    /// Linux Vulkan port stay in lockstep on PiP slide timing.
    #[test]
    fn pip_slide_progress_is_ease_out_cubic() {
        let mut state = LoopState::new(BlendingCompositorConfig {
            pip_slide_duration: 1.0,
            ..Default::default()
        });
        // No animation → 0.
        assert_eq!(state.pip_slide_progress(), 0.0);

        state.pip_animation_start = Some(Instant::now() - Duration::from_millis(250));
        let q1 = state.pip_slide_progress();
        // ease-out-cubic at t=0.25: 1 - (0.75)^3 ≈ 0.578
        assert!(
            (q1 - 0.578).abs() < 0.05,
            "expected ~0.578 at t=0.25, got {q1}"
        );

        state.pip_animation_start = Some(Instant::now() - Duration::from_millis(2000));
        let done = state.pip_slide_progress();
        assert!((done - 1.0).abs() < 1e-3, "expected 1.0 past duration, got {done}");
    }
}
