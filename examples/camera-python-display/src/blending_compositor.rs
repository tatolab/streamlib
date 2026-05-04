// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Blending Compositor — multi-layer alpha-over composite with PiP slide-in
//! on the canonical graphics-kernel + texture-cache RHI.
//!
//! Runs as a [`ManualProcessor`] with a render thread paced against the
//! display's refresh rate (60 Hz fallback). Each tick reads the latest
//! frame from each input port (older queued frames are dropped by the
//! port's `SkipToLatest` read mode), resolves the input frames'
//! [`StreamTexture`]s via `GpuContext::resolve_videoframe_registration`
//! (Path 1 — same-process texture cache), picks the next slot in a
//! ring of pre-allocated render-target output `StreamTexture`s,
//! dispatches the compositor's graphics kernel into it, and emits the
//! slot's surface UUID downstream.
//!
//! Layer order (bottom → top): video → lower_third → watermark → PiP.
//!
//! Linux-only. The pre-RHI macOS Metal path was removed when the
//! compositor was rewritten on `VulkanBlendingCompositor` /
//! `VulkanGraphicsKernel` (#485) — supporting it would have required
//! parallel adapter machinery that does not exist outside the engine
//! today.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use streamlib::core::display_info;
use streamlib::core::rhi::{StreamTexture, TextureFormat, VulkanLayout};
use streamlib::core::{
    GpuContextLimitedAccess, Result, RuntimeContextFullAccess, StreamError,
};
use streamlib::iceoryx2::{InputMailboxes, OutputWriter};
use streamlib::{
    BlendingCompositorInputs, BlendingLayer, BlendingOutput, Videoframe,
    VulkanBlendingCompositor,
};

/// Iteration cap applied when [`BlendingCompositorConfig::target_fps`]
/// or the display refresh query produces a non-positive value.
const FALLBACK_TARGET_FPS: f64 = 60.0;

/// Render-loop iteration count slack: at 60 Hz nominal we tolerate up
/// to ~5 fps over (drift between sleep granularity + scheduler) before
/// the loop is considered "out of bound" — issue exit-criterion
/// "60 → ≤ 65/s on a 60 Hz display".
#[cfg_attr(not(test), allow(dead_code))]
const TARGET_FPS_OVERSHOOT_SLACK: f64 = 5.0;

/// Slow-tick threshold restored from the pre-rewrite `STUTTER!` log
/// (50 ms ≈ three frames at 60 Hz). Exceeding this is a clear hitch
/// worth surfacing even when the loop's per-tick cadence still
/// averages out.
const SLOW_TICK_WARN_THRESHOLD: Duration = Duration::from_millis(50);

/// Output texture ring depth — matches the engine's standard
/// frames-in-flight (display, encoders) per
/// `docs/learnings/vulkan-frames-in-flight.md`. Display reads slot N
/// while the compositor is rendering slot N+1; with 2 slots the
/// producer never overwrites a texture the consumer is sampling.
const OUTPUT_RING_DEPTH: usize = 2;

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

/// Output ring slot — pre-allocated render-target texture + the UUID
/// it is registered under in `GpuContext::texture_cache`.
struct OutputSlot {
    surface_id: String,
    texture: StreamTexture,
}

/// GPU backend bundle owned by the processor and moved into the
/// render thread on `start()`. The output texture ring is allocated
/// during `setup()` (FullAccess required for
/// `acquire_render_target_dma_buf_image`) and consumed read-only by
/// the render thread (LimitedAccess is sufficient for resolving
/// registrations).
struct GpuBackend {
    compositor: Arc<VulkanBlendingCompositor>,
    output_ring: Vec<OutputSlot>,
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

    /// Backend instantiated in `setup()` and moved into the render
    /// thread by `start()`.
    backend: Option<GpuBackend>,
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
        let outputs = Arc::clone(&self.outputs);
        let running = Arc::clone(&self.running);
        let frame_count = Arc::clone(&self.frame_count);
        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Configuration("setup() not run".into()))?;
        let backend = self
            .backend
            .take()
            .ok_or_else(|| StreamError::Configuration("setup() not run".into()))?;
        let config = self.config.clone();

        running.store(true, Ordering::Release);

        let handle = std::thread::Builder::new()
            .name("blending-compositor".into())
            .spawn(move || {
                let mut state = LoopState::new(config);
                manual_render_loop(target_fps, Arc::clone(&running), || {
                    if let Err(e) = compose_one_frame(
                        &mut state,
                        &gpu_context,
                        &inputs,
                        &outputs,
                        &frame_count,
                        &backend,
                    ) {
                        tracing::warn!("BlendingCompositor: tick failed: {e}");
                    }
                });
                tracing::info!(
                    "BlendingCompositor: stopped ({} frames)",
                    frame_count.load(Ordering::Relaxed)
                );
            })
            .map_err(|e| {
                StreamError::Configuration(format!("spawn render thread: {e}"))
            })?;
        self.render_thread = Some(handle);
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl BlendingCompositorProcessor::Processor {
    fn resolve_target_fps(&self) -> f64 {
        if let Some(fps) = self.config.target_fps {
            if fps > 0.0 {
                return fps;
            }
        }
        // Render thread runs without a window handle; the underlying
        // helper falls back to the primary monitor's refresh on Linux,
        // returning a positive `f64` directly.
        let rate = display_info::get_refresh_rate(None);
        if rate > 0.0 {
            rate
        } else {
            FALLBACK_TARGET_FPS
        }
    }

    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("BlendingCompositor: setup (Vulkan)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        let gpu_full = ctx.gpu_full_access();
        let vulkan_device = gpu_full.device().vulkan_device().clone();
        let compositor = Arc::new(VulkanBlendingCompositor::new(&vulkan_device)?);

        // Pre-allocate the output texture ring — render-target-capable
        // tiled DMA-BUF VkImages, registered in
        // `GpuContext::texture_cache` so downstream consumers
        // (`LinuxDisplayProcessor`, future encoders) resolve them via
        // the standard Path 1 lookup.
        //
        // UUIDs encode both the processor ("blending_compositor") and
        // the slot index so a future debugger reading the registry
        // sees what the surface_id actually maps to without a grep.
        let mut output_ring: Vec<OutputSlot> = Vec::with_capacity(OUTPUT_RING_DEPTH);
        for slot_idx in 0..OUTPUT_RING_DEPTH {
            let texture = gpu_full.acquire_render_target_dma_buf_image(
                self.config.width,
                self.config.height,
                TextureFormat::Bgra8Unorm,
            )?;
            // Engine UUIDv4-shaped fixed string per slot — keeps the
            // `surface_id` stable across runs (helpful for log
            // correlation) and the slot index visible in the last
            // octet so a tail of warnings names the slot in flight.
            let surface_id =
                format!("00000000-0000-0000-0000-0000blendc{slot_idx:03}");
            // Compositor's post-render barrier leaves the texture in
            // SHADER_READ_ONLY_OPTIMAL — declare that as the registered
            // current layout so downstream consumers issue zero-cost
            // (no-op) read barriers from the steady-state second
            // dispatch onward. The very first dispatch into a slot
            // reads the registration as UNDEFINED (because it has yet
            // to be rendered to) and the compositor's input/output
            // barrier code handles the transition. We declare
            // UNDEFINED here so the registration matches the actual
            // Vulkan tracker state pre-first-render; the compositor
            // updates the layout (via `update_layout`) after each
            // render.
            gpu_full.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            output_ring.push(OutputSlot {
                surface_id,
                texture,
            });
        }
        tracing::info!(
            "BlendingCompositor: pre-allocated {OUTPUT_RING_DEPTH} output ring slots ({}x{} BGRA8)",
            self.config.width,
            self.config.height
        );

        self.backend = Some(GpuBackend {
            compositor,
            output_ring,
        });
        Ok(())
    }
}

// ---- Render-loop scaffolding ------------------------------------------------

/// Per-iteration state owned by the spawned render thread.
struct LoopState {
    config: BlendingCompositorConfig,
    pip_ready: bool,
    pip_animation_start: Option<Instant>,
    first_video_time: Option<Instant>,
    cached_video_dimensions: Option<(u32, u32)>,
    /// Round-robin index into [`GpuBackend::output_ring`].
    next_output_slot: usize,
}

impl LoopState {
    fn new(config: BlendingCompositorConfig) -> Self {
        Self {
            config,
            pip_ready: false,
            pip_animation_start: None,
            first_video_time: None,
            cached_video_dimensions: None,
            next_output_slot: 0,
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

/// Render loop. Sleeps after each tick to maintain `target_fps` cadence;
/// exits when `running` is cleared. Identical pacing logic as the
/// pre-rewrite version (the macOS-Metal/Linux-Vulkan split lived
/// inside `compose_one_frame`, not here).
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
            // tick spends longer than `frame_period`.
            next_deadline = now;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compose_one_frame(
    state: &mut LoopState,
    gpu_ctx: &GpuContextLimitedAccess,
    inputs: &InputMailboxes,
    outputs: &Arc<OutputWriter>,
    frame_count: &Arc<AtomicU64>,
    backend: &GpuBackend,
) -> Result<()> {
    // Resolve each upstream layer's texture + current layout via the
    // engine's `resolve_videoframe_registration` (Path 1 same-process
    // texture cache). Each Resolved entry is cloned out of the
    // registration's `Arc` so the layout is captured at this instant
    // — the compositor's input barrier reads from `current_layout`
    // and barriers to SHADER_READ_ONLY_OPTIMAL inside its render
    // submit.
    let video = read_layer(gpu_ctx, inputs, "video_in")?;
    let lower_third = read_layer(gpu_ctx, inputs, "lower_third_in")?;
    let watermark = read_layer(gpu_ctx, inputs, "watermark_in")?;
    let pip = read_layer(gpu_ctx, inputs, "pip_in")?;

    if let Some(ref v) = video {
        state.cached_video_dimensions = Some((v.texture.width(), v.texture.height()));
        if state.first_video_time.is_none() {
            state.first_video_time = Some(Instant::now());
        }
    }

    state.maybe_promote_pip(Instant::now());
    let pip_slide_progress = state.pip_slide_progress();

    let (width, height) = state
        .cached_video_dimensions
        .unwrap_or((state.config.width, state.config.height));

    // Pick the next ring slot. The previous tick's slot is N-1 (which
    // display may still be sampling); we render into N. With ring
    // depth = 2, slots alternate every frame.
    let slot_idx = state.next_output_slot;
    state.next_output_slot = (slot_idx + 1) % backend.output_ring.len();
    let slot = &backend.output_ring[slot_idx];

    // Resolve the slot's current layout from its registration. The
    // compositor's pre-render barrier reads from this layout; on the
    // very first dispatch into a slot the layout is UNDEFINED (initial
    // declaration); on subsequent cycles it is SHADER_READ_ONLY_OPTIMAL
    // (left there by the prior render's post-barrier).
    let output_registration =
        gpu_ctx.resolve_videoframe_registration(&slot_videoframe(
            &slot.surface_id,
            width,
            height,
        ))?;
    let output_current_layout = output_registration.current_layout();

    // Dispatch — the compositor records input barriers + render +
    // output barrier in one CB, submits, and waits before returning.
    backend.compositor.dispatch(BlendingCompositorInputs {
        video: video.as_ref().map(|l| l.as_layer()),
        lower_third: lower_third.as_ref().map(|l| l.as_layer()),
        watermark: watermark.as_ref().map(|l| l.as_layer()),
        pip: if state.pip_ready {
            pip.as_ref().map(|l| l.as_layer())
        } else {
            None
        },
        output: BlendingOutput {
            texture: &slot.texture,
            current_layout: output_current_layout,
        },
        pip_slide_progress,
    })?;

    // Compositor leaves all bound textures in SHADER_READ_ONLY_OPTIMAL
    // — update each registration so the next consumer's barrier reads
    // a current layout matching reality (per
    // `docs/architecture/texture-registration.md` consumer rules).
    output_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    for layer in [&video, &lower_third, &watermark, &pip].into_iter().flatten() {
        layer
            .registration
            .update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    }

    // Emit the slot's surface_id. Display resolves it via Path 1 since
    // we registered it in the texture cache at setup time.
    let count = frame_count.fetch_add(1, Ordering::Relaxed);
    let timestamp_ns = (count as i64) * 16_666_667;
    let output_frame = Videoframe {
        surface_id: slot.surface_id.clone(),
        width,
        height,
        timestamp_ns: timestamp_ns.to_string(),
        frame_index: count.to_string(),
        fps: None,
        // Per-frame override is opt-in (#633); the per-surface
        // `current_image_layout` published via surface-share / Path 1
        // is the default.
        texture_layout: None,
    };
    outputs.write("video_out", &output_frame)?;

    Ok(())
}

/// One resolved input layer — texture + the registration its
/// `current_layout` came from. Holding the `Arc<TextureRegistration>`
/// lets the compositor update layout state (via
/// [`TextureRegistration::update_layout`]) after the render submit
/// completes.
struct ResolvedLayer {
    registration: Arc<streamlib::core::context::TextureRegistration>,
    texture: StreamTexture,
}

impl ResolvedLayer {
    fn as_layer(&self) -> BlendingLayer<'_> {
        BlendingLayer {
            texture: &self.texture,
            current_layout: self.registration.current_layout(),
        }
    }
}

fn read_layer(
    gpu_ctx: &GpuContextLimitedAccess,
    inputs: &InputMailboxes,
    port: &str,
) -> Result<Option<ResolvedLayer>> {
    if !inputs.has_data(port) {
        return Ok(None);
    }
    let frame: Videoframe = inputs.read(port)?;
    let registration = gpu_ctx.resolve_videoframe_registration(&frame)?;
    let texture = registration.texture().clone();
    Ok(Some(ResolvedLayer {
        registration,
        texture,
    }))
}

/// Synthesize a Videoframe pointing at one of our output ring slots —
/// used to look up its registration for layout reads. The slot was
/// registered at setup time, so Path 1 resolves it without IPC.
fn slot_videoframe(surface_id: &str, width: u32, height: u32) -> Videoframe {
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
