// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Blending Compositor — multi-layer alpha-over composite with PiP slide-in
//! on the canonical graphics-kernel + texture-cache RHI.
//!
//! Runs as a [`ManualProcessor`] with a render thread paced against the
//! display's refresh rate (60 Hz fallback). Each tick reads the latest
//! frame from each input port (older queued frames are dropped by the
//! port's `SkipToLatest` read mode), resolves the input frames'
//! [`Texture`]s via `GpuContext::resolve_texture_registration_by_surface_id`
//! (Path 1 — same-process texture cache), picks the next slot in a
//! ring of pre-allocated render-target output `Texture`s,
//! dispatches the compositor's graphics kernel into it, and emits the
//! slot's surface UUID downstream.
//!
//! Layer order (bottom → top): video → lower_third → watermark → PiP.
//!
//! Linux-only. The macOS Metal path was retired when the compositor
//! moved onto the graphics-kernel + texture-cache RHI; supporting it
//! would have required parallel adapter machinery that does not exist
//! outside the engine today.
//!
//! The kernel wrapper itself ([`SandboxedBlendingCompositor`]) lives
//! in `blending_compositor_kernel.rs` — see that file's module-level
//! doc for why it lives in the example and not the engine.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use streamlib::sdk::engine::host_rhi::{HostVulkanTexture, HostVulkanTimelineSemaphore};
use streamlib::sdk::engine::{HostSurfaceStoreExt, HostTextureExt};

use streamlib::sdk::color::TransferId;
use streamlib::sdk::display_info;
use streamlib::sdk::rhi::{
    RhiToneMapper, Texture, TextureDescriptor, TextureFormat, TextureUsages, ToneCurveId,
    ToneMapperPushConstants, VulkanLayout,
};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::error::{Result, Error};
use streamlib::sdk::iceoryx2::{InputMailboxes, OutputWriter};
use crate::_generated_::tatolab__core::color_info::{Matrix, Primaries, Range, Transfer};
use crate::_generated_::{ColorInfo, VideoFrame};

// Sandboxed kernel wrapper — see `blending_compositor_kernel.rs` for
// the rationale (sandboxed scenario content rides the engine RHI's
// cdylib-safe surfaces).
use crate::blending_compositor_kernel::{
    BlendingCompositorInputs, BlendingLayer, BlendingOutput, SandboxedBlendingCompositor,
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
    /// Working-space `ColorInfo` for the per-acquire compositing model
    /// (per `docs/research/color-management-pipeline.md` § 2): each
    /// input frame whose declared `ColorInfo` differs from this is
    /// converted via [`RhiToneMapper`] into a per-port intermediate
    /// before the composite kernel reads it; the output frame stamps
    /// this same `ColorInfo`.
    ///
    /// When unset, defaults to sRGB BT.709 / Identity / Full — matches
    /// the implicit working space the composite kernel ingests today
    /// (RGBA8 sRGB-encoded), so all-SDR pipelines see zero conversion
    /// overhead and unchanged output.
    #[serde(default)]
    pub working_space_color: Option<ColorInfo>,
    /// Peak luminance (cd/m²) the working-space `ColorInfo` references.
    /// Drives the BT.2390 / BT.2446a peak-rescale math when conversion
    /// engages. Defaults to 100 nits (SDR diffuse-white reference).
    #[serde(default)]
    pub working_space_peak_nits: Option<f32>,
    /// Tone curve applied when an input's `ColorInfo` differs from the
    /// working space. Defaults to BT.2390 (HDR→SDR) — the common case
    /// for HDR sources targeting an SDR working space.
    #[serde(default)]
    pub default_tone_curve: Option<ToneCurveSelector>,
}

/// Serializable proxy for [`ToneCurveId`] so the engine-internal enum
/// can be set from config YAML / JSON without leaking the engine type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToneCurveSelector {
    /// Identity — pure transfer rescale, no tone curve.
    None,
    /// ITU-R BT.2390 EETF — HDR→SDR Hermite spline in PQ space.
    Bt2390,
    /// ITU-R BT.2446-1 method A2 inverse — SDR→HDR gamma-knee.
    Bt2446a,
}

impl From<ToneCurveSelector> for ToneCurveId {
    fn from(s: ToneCurveSelector) -> Self {
        match s {
            ToneCurveSelector::None => ToneCurveId::None,
            ToneCurveSelector::Bt2390 => ToneCurveId::Bt2390,
            ToneCurveSelector::Bt2446a => ToneCurveId::Bt2446a,
        }
    }
}

impl Default for BlendingCompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            pip_slide_duration: 0.5,
            pip_slide_delay: 2.5,
            target_fps: None,
            working_space_color: None,
            working_space_peak_nits: None,
            default_tone_curve: None,
        }
    }
}

/// Output ring slot — pre-allocated render-target texture + the UUID
/// it is registered under in `GpuContext::texture_cache`.
struct OutputSlot {
    surface_id: String,
    texture: Texture,
    // Per-slot single-writer-per-edge timeline pair held on the host
    // side so cross-process consumers reaching the slot via
    // `surface_store.lookup` see live timeline FDs (the registration
    // duplicated them via SCM_RIGHTS but the host-side Arcs must
    // outlive the surface). See
    // `docs/architecture/adapter-timeline-single-writer.md`.
    _produce_done: Arc<HostVulkanTimelineSemaphore>,
    _consume_done: Arc<HostVulkanTimelineSemaphore>,
}

/// GPU backend bundle owned by the processor and moved into the
/// render thread on `start()`. The output texture ring is allocated
/// during `setup()` (FullAccess required for
/// `acquire_render_target_dma_buf_image`) and consumed read-only by
/// the render thread (LimitedAccess is sufficient for resolving
/// registrations).
struct GpuBackend {
    compositor: Arc<SandboxedBlendingCompositor>,
    output_ring: Vec<OutputSlot>,
    /// Per-input tone-mapper. Constructed in `setup()` (kernel is
    /// allocated lazily on first dispatch). Engaged by
    /// `normalize_layer` when an input frame's `ColorInfo` differs
    /// from the working space.
    tone_mapper: Arc<RhiToneMapper>,
    /// Per-port intermediate textures, lazily allocated on first
    /// frame and reallocated when input dimensions change. Keyed by
    /// port name ("video_in", "lower_third_in", etc.). Only the
    /// render thread mutates this; the mutex exists so the map can
    /// also be inspected from other threads if a debug surface is
    /// added later.
    intermediates: StdMutex<HashMap<String, Intermediate>>,
    /// Host Vulkan device, threaded through for lazy intermediate
    /// allocation. The render thread holds the only consumer-side
    /// `Arc`; teardown releases it when the thread exits.
    vulkan_device: Arc<streamlib::sdk::engine::host_rhi::HostVulkanDevice>,
}

/// Per-input intermediate texture used by the per-acquire tone-mapping
/// stage. The compositor reads this when the input's `ColorInfo`
/// differs from the working space; allocated lazily on first
/// conversion at the input's dimensions and reallocated on dimension
/// change.
struct Intermediate {
    texture: Texture,
    width: u32,
    height: u32,
    /// Last-known Vulkan layout the texture is in. Tracked by the
    /// tone-mapper's `apply_with_layouts` which leaves the texture in
    /// `SHADER_READ_ONLY_OPTIMAL` after every dispatch.
    current_layout: VulkanLayout,
}

#[streamlib::sdk::processor("BlendingCompositor")]
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

impl streamlib::sdk::processors::ManualProcessor for BlendingCompositorProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.setup_inner(ctx)
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "BlendingCompositor: teardown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let target_fps = self.resolve_target_fps();
        tracing::info!(
            "BlendingCompositor: starting render thread @ {:.1} fps",
            target_fps
        );

        let inputs = std::mem::take(&mut self.inputs);
        let outputs = self.outputs.clone();
        let running = Arc::clone(&self.running);
        let frame_count = Arc::clone(&self.frame_count);
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            Error::Configuration(
                "BlendingCompositor::start: gpu_context unset (setup() not run)".into(),
            )
        })?;
        let backend = self.backend.take().ok_or_else(|| {
            Error::Configuration(
                "BlendingCompositor::start: backend unset (setup() not run)".into(),
            )
        })?;
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
                Error::Configuration(format!("spawn render thread: {e}"))
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
        let gpu_context = ctx.gpu_limited_access().clone();
        self.gpu_context = Some(gpu_context.clone());

        // setup() runs inside the engine's privileged lifecycle dispatch
        // (`ProcessorInstance::setup`), so `ctx.gpu_full_access()` is
        // already privileged in both cdylib and in-process modes: cdylib
        // bodies see a ScopeToken-shaped FullAccess routed through the
        // FullAccess vtable, in-process bodies see the Boxed FullAccess
        // dispatched directly. Calling `gpu_limited_access().escalate(...)`
        // here would re-enter the same gate and trip the gate's same-
        // thread re-entry panic.
        //
        // `host_vulkan_device_arc()` is the cdylib-safe FullAccess
        // accessor; the `Arc<HostVulkanDevice>` it returns is held by
        // the sandboxed graphics-kernel wrapper for steady-state
        // dispatch.
        let width = self.config.width;
        let height = self.config.height;
        let full = ctx.gpu_full_access();
        let vulkan_device = full.host_vulkan_device_arc()?;
        let compositor = Arc::new(SandboxedBlendingCompositor::new(&vulkan_device)?);

        // Pre-allocate the output texture ring — render-target-capable
        // tiled DMA-BUF VkImages. Dual-registration happens below via
        // the LimitedAccess `register_texture_with_layout` /
        // `surface_store` primitives.
        let mut ring_descriptors: Vec<(String, Texture)> =
            Vec::with_capacity(OUTPUT_RING_DEPTH);
        for slot_idx in 0..OUTPUT_RING_DEPTH {
            let texture = full.acquire_render_target_dma_buf_image(
                width,
                height,
                TextureFormat::Bgra8Unorm,
            )?;
            // Engine UUIDv4-shaped fixed string per slot — keeps the
            // `surface_id` stable across runs (helpful for log
            // correlation) and the slot index visible in the last
            // octet so a tail of warnings names the slot in flight.
            // Hex-only by construction so any future consumer that
            // parses surface_id as a real UUID still resolves it
            // (`b1e0d` ≈ "blend").
            let surface_id =
                format!("00000000-0000-0000-0000-00000b1e0d{slot_idx:02x}");
            ring_descriptors.push((surface_id, texture));
        }

        // Per-input tone-mapper. Cheap to construct (kernel built
        // lazily on first dispatch); each consumer owns its own
        // instance per the "no engine-side cache" pattern.
        let tone_mapper = Arc::new(RhiToneMapper::new(&vulkan_device));

        // Dual-register each slot outside `escalate`:
        // - `GpuContext::texture_cache` (Path 1 — in-process consumers
        //   like `LinuxDisplayProcessor` and `CrtFilmGrain`).
        // - `surface_store` (Path 2 — cross-process consumers like the
        //   `cyberpunk_glitch` Python subprocess reaching the ring via
        //   `OpenGLContext.acquire_read`).
        //
        // The two registrations describe the same texture and declare
        // matching layouts (anti-pattern #2 in `texture-registration.md`
        // — never let descriptor-side claims diverge from registration).
        // Path 1 starts at `UNDEFINED` (the compositor's barrier code
        // handles the first-render transition); Path 2 declares
        // `SHADER_READ_ONLY_OPTIMAL` because Glitch reads after the
        // first render lands.
        let mut output_ring: Vec<OutputSlot> = Vec::with_capacity(OUTPUT_RING_DEPTH);
        for (slot_idx, (surface_id, texture)) in ring_descriptors.into_iter().enumerate()
        {
            // Per-slot single-writer-per-edge exportable timelines —
            // `produce_done` signaled by the host-side compositor,
            // `consume_done` signaled by cross-process consumers (Glitch
            // Python subprocess). See
            // `docs/architecture/adapter-timeline-single-writer.md`.
            let produce_done = Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(vulkan_device.device(), 0)
                    .map_err(|e| {
                        Error::Configuration(format!(
                            "BlendingCompositor: HostVulkanTimelineSemaphore::new_exportable \
                             (produce_done) slot {slot_idx}: {e}"
                        ))
                    })?,
            );
            let consume_done = Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(vulkan_device.device(), 0)
                    .map_err(|e| {
                        Error::Configuration(format!(
                            "BlendingCompositor: HostVulkanTimelineSemaphore::new_exportable \
                             (consume_done) slot {slot_idx}: {e}"
                        ))
                    })?,
            );
            gpu_context.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            let surface_store = gpu_context.surface_store().ok_or_else(|| {
                Error::Configuration(
                    "BlendingCompositor: GpuContext has no surface_store \
                     — cross-process output (Glitch consumer) unavailable"
                        .to_string(),
                )
            })?;
            surface_store
                .register_texture(
                    &surface_id,
                    &texture,
                    Some(produce_done.as_ref()),
                    Some(consume_done.as_ref()),
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                )
                .map_err(|e| {
                    Error::Configuration(format!(
                        "BlendingCompositor: surface_store.register_texture \
                         slot {slot_idx}: {e}"
                    ))
                })?;
            output_ring.push(OutputSlot {
                surface_id,
                texture,
                _produce_done: produce_done,
                _consume_done: consume_done,
            });
        }
        tracing::info!(
            "BlendingCompositor: pre-allocated {OUTPUT_RING_DEPTH} output ring slots ({width}x{height} BGRA8)"
        );

        self.backend = Some(GpuBackend {
            compositor,
            output_ring,
            tone_mapper,
            intermediates: StdMutex::new(HashMap::new()),
            vulkan_device,
        });
        Ok(())
    }
}

// ---- Render-loop scaffolding ------------------------------------------------

/// Per-iteration state owned by the spawned render thread.
///
/// The four `last_*` fields cache each input port's most recently
/// resolved layer so a tick where iceoryx2 has no fresh `has_data` for
/// that port still composites against the producer's last-known
/// surface — visual continuity instead of a one-frame layer drop. The
/// camera (~30 fps), the two Skia generators (60 fps), and the
/// compositor itself (60 fps) all run on independent clocks; without
/// the cache, any tick with imperfect alignment briefly drops a
/// layer and the user sees a flicker.
///
/// The texture pointed at by a cached registration is still live —
/// producers write into the same `surface_id` (or rotate through a
/// ring keyed by `frame_index`), so a cached resolve names whatever
/// the producer most recently wrote. Layout drift is harmless: the
/// compositor's pre-render barrier transitions from
/// `current_layout` regardless of how stale the value is.
struct LoopState {
    config: BlendingCompositorConfig,
    pip_ready: bool,
    pip_animation_start: Option<Instant>,
    first_video_time: Option<Instant>,
    cached_video_dimensions: Option<(u32, u32)>,
    /// Round-robin index into [`GpuBackend::output_ring`].
    next_output_slot: usize,
    last_video: Option<ResolvedLayer>,
    last_lower_third: Option<ResolvedLayer>,
    last_watermark: Option<ResolvedLayer>,
    last_pip: Option<ResolvedLayer>,
    /// Working-space `ColorInfo` resolved from config. Inputs whose
    /// declared `ColorInfo` differs are converted into this space by
    /// `normalize_layer` before the composite kernel reads them. The
    /// output frame stamps this value.
    working_space_color_info: ColorInfo,
    /// Working-space reference peak luminance (cd/m²).
    working_space_peak_nits: f32,
    /// Tone curve dispatched when an input's `ColorInfo` differs from
    /// the working space.
    default_tone_curve: ToneCurveId,
}

impl LoopState {
    fn new(config: BlendingCompositorConfig) -> Self {
        let working_space_color_info = config
            .working_space_color
            .clone()
            .unwrap_or_else(default_working_space);
        let working_space_peak_nits = config.working_space_peak_nits.unwrap_or(100.0);
        let default_tone_curve = config
            .default_tone_curve
            .map(ToneCurveId::from)
            .unwrap_or(ToneCurveId::Bt2390);
        Self {
            config,
            pip_ready: false,
            pip_animation_start: None,
            first_video_time: None,
            cached_video_dimensions: None,
            next_output_slot: 0,
            last_video: None,
            last_lower_third: None,
            last_watermark: None,
            last_pip: None,
            working_space_color_info,
            working_space_peak_nits,
            default_tone_curve,
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
    outputs: &OutputWriter,
    frame_count: &Arc<AtomicU64>,
    backend: &GpuBackend,
) -> Result<()> {
    // Resolve each upstream layer's texture + current layout via the
    // engine's `resolve_texture_registration_by_surface_id` (Path 1 same-process
    // texture cache). When a port has no new frame this tick (the
    // producer's clock didn't align with ours), reuse the prior
    // tick's resolved layer — see [`LoopState`] for the rationale.
    refresh_layer(gpu_ctx, inputs, "video_in", &mut state.last_video)?;
    refresh_layer(gpu_ctx, inputs, "lower_third_in", &mut state.last_lower_third)?;
    refresh_layer(gpu_ctx, inputs, "watermark_in", &mut state.last_watermark)?;
    refresh_layer(gpu_ctx, inputs, "pip_in", &mut state.last_pip)?;

    // Per-input tone-mapping normalization — per-acquire conversion
    // into the working-space ColorInfo before the composite kernel
    // reads each layer. Passthrough when input already matches the
    // working space (the all-SDR default case for current pipelines).
    for (port, slot) in [
        ("video_in", state.last_video.as_mut()),
        ("lower_third_in", state.last_lower_third.as_mut()),
        ("watermark_in", state.last_watermark.as_mut()),
        ("pip_in", state.last_pip.as_mut()),
    ] {
        if let Some(layer) = slot {
            normalize_layer(
                port,
                layer,
                &state.working_space_color_info,
                state.working_space_peak_nits,
                state.default_tone_curve,
                &backend.tone_mapper,
                &backend.intermediates,
                &backend.vulkan_device,
            )?;
        }
    }

    if let Some(v) = state.last_video.as_ref() {
        state.cached_video_dimensions = Some((v.texture.width(), v.texture.height()));
        if state.first_video_time.is_none() {
            state.first_video_time = Some(Instant::now());
        }
    }

    state.maybe_promote_pip(Instant::now());
    let pip_slide_progress = state.pip_slide_progress();
    let pip_ready = state.pip_ready;

    let (width, height) = state
        .cached_video_dimensions
        .unwrap_or((state.config.width, state.config.height));

    // Pick the next ring slot. The previous tick's slot is N-1 (which
    // display may still be sampling); we render into N. With ring
    // depth = 2, slots alternate every frame.
    let slot_idx = state.next_output_slot;
    state.next_output_slot = (slot_idx + 1) % backend.output_ring.len();
    let slot = &backend.output_ring[slot_idx];

    // Resolve the slot's registration so we can `update_layout` after
    // the dispatch returns. The compositor's `offscreen_render` starts
    // from `UNDEFINED` internally (content discard permitted, full-
    // screen triangle overwrites every pixel), so it doesn't read the
    // slot's prior layout — we just need the registration handle.
    let output_registration = {
        let synth = slot_videoframe(&slot.surface_id, width, height);
        gpu_ctx.resolve_texture_registration_by_surface_id(
            &synth.surface_id,
            synth.texture_layout,
            synth.width,
            synth.height,
        )?
    };

    // Borrow each cached layer immutably for the dispatch — `state`
    // is no longer mutated past this point.
    let video = state.last_video.as_ref();
    let lower_third = state.last_lower_third.as_ref();
    let watermark = state.last_watermark.as_ref();
    let pip = state.last_pip.as_ref();

    // Dispatch — the compositor records input barriers (when needed) +
    // offscreen render + output post-barrier through the engine RHI,
    // submits each, and waits before returning.
    backend.compositor.dispatch(BlendingCompositorInputs {
        video: video.map(|l| l.as_layer()),
        lower_third: lower_third.map(|l| l.as_layer()),
        watermark: watermark.map(|l| l.as_layer()),
        pip: if pip_ready {
            pip.map(|l| l.as_layer())
        } else {
            None
        },
        output: BlendingOutput { texture: &slot.texture },
        pip_slide_progress,
    })?;

    // Compositor leaves all bound textures in SHADER_READ_ONLY_OPTIMAL
    // — update each registration so the next consumer's barrier reads
    // a current layout matching reality (per
    // `docs/architecture/texture-registration.md` consumer rules).
    output_registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    for layer in [video, lower_third, watermark, pip].into_iter().flatten() {
        layer
            .registration
            .update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    }

    // Emit the slot's surface_id. Display resolves it via Path 1 since
    // we registered it in the texture cache at setup time.
    let count = frame_count.fetch_add(1, Ordering::Relaxed);
    let timestamp_ns = (count as i64) * 16_666_667;
    // Output ColorInfo stamps the working-space — every input was
    // converted into this space by `normalize_layer` (or the working
    // space matched the input and passthrough happened, equivalent).
    // Either way, the output bytes the composite kernel wrote are in
    // working-space encoding, so stamping that is honest.
    let output_color_info = Some(state.working_space_color_info.clone());
    let output_frame = VideoFrame {
        surface_id: slot.surface_id.clone(),
        width,
        height,
        timestamp_ns: timestamp_ns.to_string(),
        fps: None,
        // Per-frame override is opt-in; the per-surface
        // `current_image_layout` published via surface-share / Path 1
        // is the default.
        texture_layout: None,
        color_info: output_color_info,
        mastering_display: None,
        content_light: None,
    };
    outputs.write("video_out", &output_frame)?;

    Ok(())
}

/// Default working-space `ColorInfo` when config doesn't set one:
/// canonical sRGB BT.709 / Identity / Full. Matches the implicit
/// working space the composite kernel ingests today (RGBA8 sRGB-
/// encoded), so all-SDR pipelines see zero conversion overhead.
fn default_working_space() -> ColorInfo {
    ColorInfo {
        primaries: Some(Primaries::Bt709),
        transfer: Some(Transfer::Srgb),
        matrix: Some(Matrix::Identity),
        range: Some(Range::Full),
    }
}

/// Map BC's local schema `Transfer` enum to the engine's `TransferId`
/// push-constant id. Local because the engine's
/// [`streamlib::sdk::color::transfer_id_from_schema`] takes the
/// engine-crate's flavor of the schema enum (a distinct Rust type
/// from BC's `_generated_/` codegen output, even though both are
/// generated from the same JTD source). Match arms mirror the
/// engine helper's coverage.
fn transfer_id_from_schema(t: &Transfer) -> TransferId {
    match t {
        Transfer::Srgb => TransferId::Srgb,
        Transfer::Bt709
        | Transfer::Smpte170m
        | Transfer::Bt2020TenBit
        | Transfer::Bt2020TwelveBit => TransferId::Bt709,
        Transfer::Smpte2084 => TransferId::Pq,
        Transfer::AribStdB67 => TransferId::Hlg,
        Transfer::Linear => TransferId::Linear,
        // Gamma22 / Gamma28 / Smpte240m / Log* / Xvycc / Bt1361 /
        // Smpte428 are uncommon end-to-end; map to Linear (no transform).
        _ => TransferId::Linear,
    }
}

/// True when an input frame's `ColorInfo` matches the working space.
/// `None` axes on input are treated as matching (defaults flow through
/// to the working space — which is exactly today's behavior for the
/// many frames with no color tag at all).
fn color_info_matches_working_space(input: Option<&ColorInfo>, working: &ColorInfo) -> bool {
    let Some(input) = input else { return true };
    // Per axis: None on input means "match"; Some must equal the
    // working-space value.
    let prim_ok = input.primaries.is_none() || input.primaries == working.primaries;
    let xfer_ok = input.transfer.is_none() || input.transfer == working.transfer;
    let mtx_ok = input.matrix.is_none() || input.matrix == working.matrix;
    let rng_ok = input.range.is_none() || input.range == working.range;
    prim_ok && xfer_ok && mtx_ok && rng_ok
}

/// One resolved input layer — texture + the registration its
/// `current_layout` came from. Holding the [`TextureRegistration`]
/// (PluginAbiObject, cheap Clone via vtable refcount bump) lets the
/// compositor update layout state via
/// [`TextureRegistration::update_layout`] after the render submit
/// completes.
///
/// `source_color_info` is the frame's declared `ColorInfo` (if any) —
/// used by `normalize_layer` to detect mismatches against the
/// working space and engage the tone-mapper.
struct ResolvedLayer {
    registration: streamlib::sdk::context::TextureRegistration,
    texture: Texture,
    /// `ColorInfo` declared on the source `VideoFrame`. `None` means
    /// the producer didn't tag the frame; defaults to the working
    /// space (no conversion engages).
    source_color_info: Option<ColorInfo>,
    /// Source content peak luminance (cd/m²), if `mastering_display`
    /// / `content_light` sidecars are populated. Defaults to 100 nits
    /// for SDR sources where the field is absent.
    source_peak_nits: f32,
    /// When `normalize_layer` engages and tone-maps the source into a
    /// per-port intermediate, this points at the intermediate texture
    /// instead of `registration.texture()`. Layout state is tracked
    /// on the intermediate itself (via `Intermediate::current_layout`)
    /// rather than the `TextureRegistration` (which describes the
    /// upstream-shared texture, unrelated to our scratch space).
    normalized_layout: Option<VulkanLayout>,
}

impl ResolvedLayer {
    fn as_layer(&self) -> BlendingLayer<'_> {
        // When normalize_layer engaged, the layout came from the
        // intermediate's tracking; otherwise from the upstream
        // registration. Both cases produce a layout that satisfies
        // the compositor's pre-render barrier expectations.
        let current_layout = self
            .normalized_layout
            .unwrap_or_else(|| self.registration.current_layout());
        BlendingLayer {
            texture: &self.texture,
            current_layout,
        }
    }
}

/// Resolve `port`'s freshest videoframe (if any) and refresh the
/// caller's `last` cache. Leaves the cache untouched when no new
/// frame has arrived since the prior tick — the cache then carries
/// over the prior layer for the next dispatch.
fn refresh_layer(
    gpu_ctx: &GpuContextLimitedAccess,
    inputs: &InputMailboxes,
    port: &str,
    last: &mut Option<ResolvedLayer>,
) -> Result<()> {
    if !inputs.has_data(port) {
        return Ok(());
    }
    let frame: VideoFrame = inputs.read(port)?;
    let registration = gpu_ctx.resolve_texture_registration_by_surface_id(
        &frame.surface_id,
        frame.texture_layout,
        frame.width,
        frame.height,
    )?;
    let texture = registration.texture().clone();
    // Resolve source peak from the optional `mastering_display` /
    // `content_light` sidecars; default to 100 nits SDR diffuse-white
    // when absent. `content_light.max_cll` is the more conservative
    // signal (per-content peak); `mastering_display.max_luminance` is
    // the master-display peak. Prefer `max_cll` when present per the
    // BT.2390 spec's source-peak guidance.
    let source_peak_nits = frame
        .content_light
        .as_ref()
        .map(|cl| cl.max_cll as f32)
        .or_else(|| {
            frame
                .mastering_display
                .as_ref()
                .map(|md| md.max_luminance as f32)
        })
        .unwrap_or(100.0);
    *last = Some(ResolvedLayer {
        registration,
        texture,
        source_color_info: frame.color_info.clone(),
        source_peak_nits,
        normalized_layout: None,
    });
    Ok(())
}

/// Per-input tone-map normalization: if `layer.source_color_info`
/// (after defaults resolution) differs from the working space, run
/// the tone-mapper from the upstream texture into a per-port
/// intermediate (allocating / reallocating on dimension change) and
/// repoint the layer at the intermediate. When the source already
/// matches, leaves the layer unchanged.
///
/// The composite kernel reads RGBA8 storage images in working-space
/// encoding regardless of which path runs.
fn normalize_layer(
    port: &str,
    layer: &mut ResolvedLayer,
    working_space: &ColorInfo,
    working_peak_nits: f32,
    tone_curve: ToneCurveId,
    tone_mapper: &RhiToneMapper,
    intermediates: &StdMutex<HashMap<String, Intermediate>>,
    vulkan_device: &Arc<streamlib::sdk::engine::host_rhi::HostVulkanDevice>,
) -> Result<()> {
    // Fast-path: missing color_info or matching axes mean "use the
    // working space" — no conversion engages. This is the cheap-path
    // back-compat for every existing SDR pipeline.
    let peak_matches = (layer.source_peak_nits - working_peak_nits).abs() < 1e-3;
    if color_info_matches_working_space(layer.source_color_info.as_ref(), working_space)
        && peak_matches
    {
        return Ok(());
    }

    // Already-normalized short-circuit. `refresh_layer` resets
    // `normalized_layout` to `None` whenever a fresh upstream frame
    // lands; if it's still `Some` here, the per-port intermediate
    // already holds tone-mapped content for the current source frame
    // (BlendingCompositor ticks faster than upstream producers, so
    // many ticks reuse the cached layer). Re-running the tone-mapper
    // on the same source would call `apply_with_layouts(intermediate,
    // intermediate)` — src == dst — which trips VUID-01197 twice per
    // submit on the 4-barrier sequence.
    if layer.normalized_layout.is_some() {
        return Ok(());
    }

    let width = layer.texture.width();
    let height = layer.texture.height();

    // Acquire-or-allocate the per-port intermediate at the input's
    // current dimensions.
    let mut map = intermediates.lock().unwrap();
    let intermediate = match map.get_mut(port) {
        Some(existing) if existing.width == width && existing.height == height => existing,
        _ => {
            // Allocate fresh — either first frame for this port or
            // input dims changed and the cached intermediate is the
            // wrong size.
            let desc = TextureDescriptor {
                width,
                height,
                format: TextureFormat::Bgra8Unorm,
                usage: TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING,
                label: Some("bc-tone-mapped-intermediate"),
            };
            let host_tex = HostVulkanTexture::new(vulkan_device, &desc).map_err(|e| {
                Error::Configuration(format!(
                    "BlendingCompositor: HostVulkanTexture::new for port {port}: {e}"
                ))
            })?;
            let texture = <Texture as HostTextureExt>::from_vulkan(host_tex);
            map.insert(
                port.to_string(),
                Intermediate {
                    texture,
                    width,
                    height,
                    current_layout: VulkanLayout::UNDEFINED,
                },
            );
            map.get_mut(port).expect("just inserted")
        }
    };

    // Resolve the per-axis transfer ids. Per-channel tone-curve and
    // peak rescale ride per-frame push constants; the kernel works
    // RGBA8 storage image → RGBA8 storage image in working-space
    // encoding.
    let src_transfer = layer
        .source_color_info
        .as_ref()
        .and_then(|c| c.transfer.as_ref())
        .map(transfer_id_from_schema)
        .unwrap_or_else(|| {
            working_space
                .transfer
                .as_ref()
                .map(transfer_id_from_schema)
                .unwrap_or(TransferId::Srgb)
        });
    let dst_transfer = working_space
        .transfer
        .as_ref()
        .map(transfer_id_from_schema)
        .unwrap_or(TransferId::Srgb);

    // Dispatch: input (in registration.current_layout) → intermediate
    // (in intermediate.current_layout). apply_with_layouts records
    // the barrier dance and leaves both in SHADER_READ_ONLY_OPTIMAL.
    let src_layout = layer.registration.current_layout();
    let push = ToneMapperPushConstants::new(
        width,
        height,
        src_transfer,
        dst_transfer,
        tone_curve,
        layer.source_peak_nits,
        working_peak_nits,
    );
    tone_mapper.apply_with_layouts(
        &layer.texture,
        src_layout,
        &intermediate.texture,
        intermediate.current_layout,
        &push,
    )?;
    intermediate.current_layout = VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
    // The upstream texture is left in SHADER_READ_ONLY_OPTIMAL by
    // apply_with_layouts — update the registration so the next
    // consumer reads an honest current_layout. If the prior layout
    // already matched, this is a no-op write.
    layer
        .registration
        .update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

    // Repoint the layer at the intermediate.
    layer.texture = intermediate.texture.clone();
    layer.normalized_layout = Some(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    Ok(())
}

/// Synthesize a VideoFrame pointing at one of our output ring slots —
/// used to look up its registration for layout reads. The slot was
/// registered at setup time, so Path 1 resolves it without IPC.
fn slot_videoframe(surface_id: &str, width: u32, height: u32) -> VideoFrame {
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
    /// `LinuxDisplayProcessor` and the camera processor.
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
    /// default for video input ports). `inputs.read()` calls into
    /// `PortMailbox::pop_latest`; if a future refactor changed this
    /// primitive to FIFO behavior, the loop would silently drift to
    /// consuming stale frames.
    #[test]
    fn iceoryx2_pop_latest_skips_stale_frames() {
        use streamlib::sdk::iceoryx2::PortMailbox;

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
    /// between ticks. Uses an in-memory queue mock in place of
    /// `InputMailboxes::read()` so the test exercises the loop's
    /// per-tick consume model rather than the iceoryx2 primitive.
    #[test]
    fn render_loop_consumes_one_payload_per_tick() {
        use std::sync::Mutex;

        let target_fps: f64 = 60.0;
        let running = Arc::new(AtomicBool::new(true));
        let queue: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(vec![10, 11, 12, 13, 14]));
        let consumed: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));

        let queue_for_loop = Arc::clone(&queue);
        let consumed_clone = Arc::clone(&consumed);
        let running_clone = Arc::clone(&running);

        let handle = std::thread::spawn(move || {
            manual_render_loop(target_fps, running_clone, || {
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

    /// Easing curve sanity-check — locks the PiP slide timing so a
    /// future refactor of `LoopState::pip_slide_progress` doesn't
    /// silently change the user-visible slide-in feel.
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
