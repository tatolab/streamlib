// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A window a processor owns, and the engine's one present-loop machinery
//! behind it: resolve a named surface id, compose it onto the present target,
//! present — latest-wins, so naming no surface leaves the last one up.
//!
//! The window is minted by [`process_wide_window_event_pump`], because winit
//! permits one event loop per process. Everything past the raw-window-handle
//! seam is the engine's, and the compositor is a private field rather than a
//! surface any caller names.
//!
//! Presenting is native always and never waits on an interpreter: an owner
//! whose code cannot sit in the app process names surface ids and the engine
//! drives the loop, because a vsync deadline never crosses a process
//! boundary. No window's loop runs on the pump's thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use winit::window::Window;

use crate::core::color::{ColorTraits, HdrStaticMetadata};
use crate::core::context::{GpuContextFullAccess, GpuContextLimitedAccess, TextureRegistration};
use crate::core::error::Result;
use crate::core::rhi::pool_slot_key_of_surface_id;
use crate::core::window_event_pump::{
    CoalescedWindowEventsFromEventPump, WindowRegisteredWithEventPump,
    WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
};
use crate::vulkan::rhi::{PresentScalingMode, VulkanPresentCompositor, VulkanPresentTarget};

/// What a processor asks the engine for when it wants a window of its own.
#[derive(Debug, Clone)]
pub struct ProcessorOwnedWindowRequest {
    /// The window itself, in the pump's own vocabulary.
    pub window_registration_request: WindowRegistrationRequestFromOwningProcessor,
    /// How a named frame maps onto the window.
    pub scaling_mode_for_frame_in_window: PresentScalingMode,
}

/// One published surface a window owner names for its next present.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceNamedForPresentationOnOwnedWindow<'a> {
    /// The published surface id naming the frame to show.
    pub surface_id: &'a str,
    /// The named frame's width in pixels.
    pub source_width_in_pixels: u32,
    /// The named frame's height in pixels.
    pub source_height_in_pixels: u32,
    /// The producer's published `VkImageLayout` for this frame as the raw
    /// int32 enumerant, when it overrides the per-surface default.
    pub producer_published_texture_layout: Option<i32>,
    /// The frame's colorspace-pick input. A change from the last shown frame
    /// renegotiates the swapchain.
    pub color_traits_of_frame: Option<ColorTraits>,
    /// The frame's HDR static metadata, when it carries the sidecar. Only
    /// reaches the driver when the picked colorspace is PQ or HLG.
    pub hdr_static_metadata_of_frame: Option<HdrStaticMetadata>,
}

/// What one named surface amounted to on the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedSurfacePresentationOutcome {
    /// Composed onto the window's acquired frame and presented.
    ComposedAndPresented,
    /// The id named no resolvable texture — retired by its pool, never
    /// registered, or not shared with this process. The window keeps
    /// whatever it last presented, and the failure is logged once per pool
    /// slot rather than once per attempt.
    SurfaceIdDidNotResolve,
    /// The compositor was rebuilt for the attachment format this frame's
    /// color description made the swapchain pick, rather than drawing the
    /// frame. The next named surface draws against the rebuilt kernel.
    CompositorRebuiltForThisFramesColorDescription,
    /// The compositor could not be rebuilt for the attachment format the
    /// renegotiated swapchain picked, so this window cannot draw a frame
    /// carrying this color description — nor any later one, until the
    /// description changes.
    WindowCannotDrawThisFramesColorDescription,
    /// The window server had already invalidated the swapchain, so nothing
    /// was acquired and nothing was drawn. Nothing was reconciled either:
    /// the swapchain is recreated when the pump reports a resize, so a
    /// window invalidated without one stays undrawable until the next
    /// resize arrives.
    SwapchainWentOutOfDateSoNothingWasPresented,
}

// A cross-process owner's window is driven from a thread the engine owns, so
// both states have to be movable onto it. Asserted in the library rather than
// a test: CI compiles no test-kind target in this workspace, so a bound
// checked only there is a bound nothing checks.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<ProcessorOwnedWindowAwaitingItsPresentTarget>();
    assert_send::<ProcessorOwnedWindow>();
};

/// What renegotiating the swapchain for a new color description left the
/// window able to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorspaceRenegotiationOutcome {
    /// This frame still draws — either the renegotiated swapchain needed no
    /// compositor rebuild, or the recreate failed and the previous swapchain
    /// was kept.
    ThisFrameStillDraws,
    /// The compositor was rebuilt for a new attachment format; the next named
    /// surface draws against the rebuilt kernel.
    CompositorRebuiltSoThisFrameIsSkipped,
    /// The compositor could not be rebuilt for the new attachment format.
    CompositorCouldNotBeRebuilt,
}

/// A window owned by one processor, driven a named surface at a time.
pub struct ProcessorOwnedWindow {
    gpu_context_for_surface_resolution: GpuContextLimitedAccess,
    window_title: String,
    scaling_mode_for_frame_in_window: PresentScalingMode,
    current_width_in_physical_pixels: u32,
    current_height_in_physical_pixels: u32,
    present_target: VulkanPresentTarget,
    compositor: VulkanPresentCompositor,
    registered_window: WindowRegisteredWithEventPump,
    /// Last-applied colorspace-pick input; a change renegotiates the
    /// swapchain.
    last_applied_color_traits: Option<ColorTraits>,
    /// The pool slot of the last surface id that failed to resolve, so the
    /// failure warns once per surface instead of once per redraw — a pooled
    /// surface publishes a fresh id per frame, so keying on the id itself
    /// would warn at source cadence.
    last_unresolved_pool_slot_key: Option<String>,
    /// The last color description whose recreate failed, so the failure warns
    /// once per description instead of once per frame (each frame retries).
    last_failed_recreate_color_traits: Option<Option<ColorTraits>>,
    /// Declared after `registered_window` so it drops after it: the present
    /// target's `VkSurfaceKHR` was minted from this window's raw handle, and
    /// the registration is otherwise the window's only owner. Holding a clone
    /// keeps the platform window alive past the registration however the
    /// fields above are ordered.
    _window_kept_alive_past_the_present_surface: Arc<Window>,
}

/// A window the pump has minted, holding the request it was minted from
/// until the GPU capability that mints its present target arrives.
///
/// The two steps are separate so a caller can leave the escalate gate
/// un-held across the pump round trip: it touches no GPU and is bounded by
/// the pump's own timeout, so holding the process-wide gate across it would
/// let a wedged compositor stall every GPU escalation in the process — the
/// opposite of the bound that timeout exists to give.
pub struct ProcessorOwnedWindowAwaitingItsPresentTarget {
    registered_window: WindowRegisteredWithEventPump,
    request: ProcessorOwnedWindowRequest,
}

impl ProcessorOwnedWindowAwaitingItsPresentTarget {
    /// Ask the pump for the window, before any GPU capability is involved.
    pub fn register_on_the_process_wide_window_event_pump(
        request: ProcessorOwnedWindowRequest,
    ) -> Result<Self> {
        let registered_window = process_wide_window_event_pump()?
            .request_window_for_owning_processor(request.window_registration_request.clone())?;
        Ok(Self {
            registered_window,
            request,
        })
    }
}

impl ProcessorOwnedWindow {
    /// Mint the present target and compositor for an already-registered
    /// window.
    ///
    /// Takes `GpuContextFullAccess` rather than escalating internally: the
    /// escalate gate serialises rather than reenters, so a caller already
    /// inside `escalate(|full| …)` — every minting escalate op is — would
    /// deadlock against a nested one.
    pub fn open_present_target_for_registered_window(
        gpu_context_full_access: &GpuContextFullAccess,
        registered_window: ProcessorOwnedWindowAwaitingItsPresentTarget,
    ) -> Result<Self> {
        let ProcessorOwnedWindowAwaitingItsPresentTarget {
            registered_window,
            request,
        } = registered_window;
        let window = Arc::clone(registered_window.window_shared_with_event_pump());
        let (width, height) = registered_window.current_physical_size();
        let present_target = gpu_context_full_access.create_present_target(
            window.as_ref(),
            width,
            height,
            true,
            None,
        )?;
        let compositor =
            gpu_context_full_access.create_present_compositor(present_target.color_format())?;

        let window_title = request.window_registration_request.window_title;
        tracing::info!(
            width,
            height,
            window_title = %window_title,
            "processor-owned window: window + present target ready"
        );
        Ok(Self {
            gpu_context_for_surface_resolution: gpu_context_full_access
                .host_inner()
                .limited_access(),
            window_title,
            scaling_mode_for_frame_in_window: request.scaling_mode_for_frame_in_window,
            current_width_in_physical_pixels: width,
            current_height_in_physical_pixels: height,
            present_target,
            compositor,
            registered_window,
            last_applied_color_traits: None,
            last_unresolved_pool_slot_key: None,
            last_failed_recreate_color_traits: None,
            _window_kept_alive_past_the_present_surface: window,
        })
    }

    /// The window's current drawable size in physical pixels.
    pub fn current_extent_in_physical_pixels(&self) -> (u32, u32) {
        (
            self.current_width_in_physical_pixels,
            self.current_height_in_physical_pixels,
        )
    }

    /// Drain the pump's events for this window, apply the resize, and hand
    /// the coalesced state back so the owner can apply its own close policy.
    ///
    /// Errors when the resize left the window unable to present — the recreate
    /// itself failed, or the compositor could not be rebuilt for the format it
    /// picked. What to do about that is the owner's call.
    pub fn apply_pending_window_events(&mut self) -> Result<CoalescedWindowEventsFromEventPump> {
        let events = self.registered_window.drain_window_events_from_event_pump();
        if let Some((width, height)) = events.resized_to_physical_pixels {
            self.present_target
                .recreate(width, height, self.last_applied_color_traits.as_ref())?;
            self.current_width_in_physical_pixels = width;
            self.current_height_in_physical_pixels = height;
            self.rebuild_compositor_for_the_present_targets_format()?;
        }
        Ok(events)
    }

    /// Show one named surface on the window: renegotiate the colorspace if
    /// this frame's description changed, signal its HDR metadata, resolve the
    /// id, and compose the result onto the swapchain's next image.
    ///
    /// Errors only when the present submission itself failed; a frame the
    /// loop chose not to draw comes back as an outcome, because latest-wins
    /// means the window simply keeps the frame it already has.
    pub fn show_named_surface(
        &mut self,
        named_surface: SurfaceNamedForPresentationOnOwnedWindow<'_>,
    ) -> Result<NamedSurfacePresentationOutcome> {
        if named_surface.color_traits_of_frame != self.last_applied_color_traits {
            match self.renegotiate_colorspace_for(named_surface.color_traits_of_frame) {
                ColorspaceRenegotiationOutcome::ThisFrameStillDraws => {}
                ColorspaceRenegotiationOutcome::CompositorRebuiltSoThisFrameIsSkipped => {
                    return Ok(
                        NamedSurfacePresentationOutcome::CompositorRebuiltForThisFramesColorDescription,
                    );
                }
                ColorspaceRenegotiationOutcome::CompositorCouldNotBeRebuilt => {
                    return Ok(
                        NamedSurfacePresentationOutcome::WindowCannotDrawThisFramesColorDescription,
                    );
                }
            }
        }

        // Gated inside `set_hdr_metadata` on the picked colorspace being
        // PQ/HLG, so an SDR window ignores a frame that carries the sidecar.
        if let Some(metadata) = named_surface.hdr_static_metadata_of_frame
            && let Err(e) = self.present_target.set_hdr_metadata(&metadata)
        {
            tracing::warn!(
                error = %e,
                window_title = %self.window_title,
                "processor-owned window: set_hdr_metadata failed"
            );
        }

        let Some(registration) = self.resolve_named_surface(named_surface) else {
            return Ok(NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve);
        };

        let scaling = self.scaling_mode_for_frame_in_window;
        let compositor = &self.compositor;
        // The compositor owns the source's layout bookkeeping via the
        // registration, so a draw error after the barrier cannot leave the
        // registration stale.
        let presented = self.present_target.render_frame(|frame| {
            compositor.compose_to_present_frame(frame, &registration, scaling)
        })?;
        Ok(if presented {
            NamedSurfacePresentationOutcome::ComposedAndPresented
        } else {
            NamedSurfacePresentationOutcome::SwapchainWentOutOfDateSoNothingWasPresented
        })
    }

    /// Bring the swapchain and the compositor in line with a frame whose
    /// color description differs from the last one applied.
    ///
    /// The present target is constructed with `None` — the legacy SDR pick —
    /// so the first described frame upgrades it to whatever the priority walk
    /// picks. A recreate can flip the attachment format (SDR BGRA8 → HDR10
    /// A2B10G10R10), which is what forces the compositor's kernel to rebuild.
    fn renegotiate_colorspace_for(
        &mut self,
        color_traits: Option<ColorTraits>,
    ) -> ColorspaceRenegotiationOutcome {
        if let Err(e) = self.present_target.recreate(
            self.current_width_in_physical_pixels,
            self.current_height_in_physical_pixels,
            color_traits.as_ref(),
        ) {
            if self.last_failed_recreate_color_traits != Some(color_traits) {
                tracing::warn!(
                    error = %e,
                    window_title = %self.window_title,
                    "processor-owned window: colorspace recreate failed (keeping previous \
                     swapchain; warning once per color description)"
                );
                self.last_failed_recreate_color_traits = Some(color_traits);
            }
            return ColorspaceRenegotiationOutcome::ThisFrameStillDraws;
        }
        self.last_applied_color_traits = color_traits;
        self.last_failed_recreate_color_traits = None;

        match self.rebuild_compositor_for_the_present_targets_format() {
            Ok(true) => ColorspaceRenegotiationOutcome::CompositorRebuiltSoThisFrameIsSkipped,
            Ok(false) => ColorspaceRenegotiationOutcome::ThisFrameStillDraws,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    window_title = %self.window_title,
                    "processor-owned window: compositor rebuild failed — this window cannot \
                     draw frames carrying this color description"
                );
                ColorspaceRenegotiationOutcome::CompositorCouldNotBeRebuilt
            }
        }
    }

    /// Bring the compositor's kernel in line with the format the present
    /// target currently holds, answering whether it had to be rebuilt.
    ///
    /// Every recreate goes through here, because `recreate` re-runs the format
    /// pick against the surface's current capabilities: a window dragged onto
    /// an HDR monitor can land on a new format from a pure resize, with the
    /// colour description unchanged. A compositor left behind then rejects
    /// every later compose by format mismatch.
    fn rebuild_compositor_for_the_present_targets_format(&mut self) -> Result<bool> {
        let new_format = self.present_target.color_format();
        let rebuilt = self.compositor.ensure_attachment_format(new_format)?;
        if rebuilt {
            tracing::info!(
                ?new_format,
                window_title = %self.window_title,
                "processor-owned window: rebuilt compositor for new attachment format"
            );
        }
        Ok(rebuilt)
    }

    /// Resolve the named id through the engine's blessed order — same-process
    /// texture cache, cross-process DMA-BUF import, pixel-buffer upload —
    /// warning at most once per pool slot on failure.
    fn resolve_named_surface(
        &mut self,
        named_surface: SurfaceNamedForPresentationOnOwnedWindow<'_>,
    ) -> Option<TextureRegistration> {
        match self
            .gpu_context_for_surface_resolution
            .resolve_texture_registration_by_surface_id(
                named_surface.surface_id,
                named_surface.producer_published_texture_layout,
                named_surface.source_width_in_pixels,
                named_surface.source_height_in_pixels,
            ) {
            Ok(registration) => {
                self.last_unresolved_pool_slot_key = None;
                Some(registration)
            }
            Err(e) => {
                let unresolved_pool_slot_key =
                    pool_slot_key_of_surface_id(named_surface.surface_id);
                if self.last_unresolved_pool_slot_key.as_deref() != Some(unresolved_pool_slot_key) {
                    tracing::warn!(
                        surface_id = %named_surface.surface_id,
                        error = %e,
                        window_title = %self.window_title,
                        "processor-owned window: failed to resolve the named surface \
                         (warning once per surface)"
                    );
                    self.last_unresolved_pool_slot_key = Some(unresolved_pool_slot_key.to_string());
                }
                None
            }
        }
    }
}

/// How long the engine's present loop parks when its owner has named no new
/// surface. Short because the pump's events are drained on the same
/// iteration, so it also bounds how long a resize waits to be applied; an
/// owner that names a surface unparks the thread rather than waiting it out.
const PROCESSOR_OWNED_WINDOW_PRESENT_LOOP_IDLE_PARK_INTERVAL: Duration = Duration::from_millis(1);

/// One named surface handed to the engine's present loop, owning its id
/// because it outlives the call that named it.
#[derive(Debug, Clone)]
pub struct SurfaceNamedForTheEnginesPresentLoop {
    /// The published surface id naming the frame to show.
    pub surface_id: String,
    /// The named frame's width in pixels.
    pub source_width_in_pixels: u32,
    /// The named frame's height in pixels.
    pub source_height_in_pixels: u32,
    /// The producer's published `VkImageLayout` for this frame as the raw
    /// int32 enumerant, when it overrides the per-surface default.
    pub producer_published_texture_layout: Option<i32>,
}

impl SurfaceNamedForTheEnginesPresentLoop {
    fn as_named_surface(&self) -> SurfaceNamedForPresentationOnOwnedWindow<'_> {
        SurfaceNamedForPresentationOnOwnedWindow {
            surface_id: &self.surface_id,
            source_width_in_pixels: self.source_width_in_pixels,
            source_height_in_pixels: self.source_height_in_pixels,
            producer_published_texture_layout: self.producer_published_texture_layout,
            // The colour description does not cross the escalate wire yet, so
            // a cross-process owner's window stays on the legacy SDR pick.
            color_traits_of_frame: None,
            hdr_static_metadata_of_frame: None,
        }
    }
}

/// Everything the processor that owns a window learns about it, coalesced,
/// because polled state is the only thing that crosses a process boundary —
/// no callback does.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CoalescedProcessorOwnedWindowStateForOwningProcessor {
    /// The window's current drawable width in physical pixels.
    pub current_width_in_physical_pixels: u32,
    /// The window's current drawable height in physical pixels.
    pub current_height_in_physical_pixels: u32,
    /// Whether the user asked to close the window since the last drain.
    pub close_requested_by_user: bool,
    /// Whether the engine has closed the window. Sticky.
    pub window_is_closed: bool,
}

/// The present loop the engine runs for a window whose owner's code cannot
/// sit in the app process.
///
/// The owner names published surface ids and reads coalesced state; this
/// thread does the acquiring, composing and presenting, paced by vsync.
/// Latest-wins: several ids named between two vsyncs leave the newest
/// showing, and naming none leaves the last frame up, so the owner's pace
/// never stutters the window and no vsync deadline ever crosses the hop.
///
/// Dropping it stops the loop, joins the thread and closes the window.
pub struct WindowPresentLoopForOwningProcessor {
    latest_surface_named_by_the_owning_processor:
        Arc<Mutex<Option<SurfaceNamedForTheEnginesPresentLoop>>>,
    coalesced_state_for_the_owning_processor:
        Arc<Mutex<CoalescedProcessorOwnedWindowStateForOwningProcessor>>,
    frames_composed_and_presented: Arc<AtomicU64>,
    present_loop_keeps_running: Arc<AtomicBool>,
    /// Held separately from the join handle so naming a surface can unpark
    /// the loop without taking the lock the join needs.
    present_loop_thread_for_unparking: Thread,
    /// `None` once the thread has been joined, which is what makes closing
    /// idempotent — a window closed by its owner and one closed by a user
    /// gesture both end here.
    present_loop_thread_awaiting_its_join: Mutex<Option<JoinHandle<()>>>,
}

impl WindowPresentLoopForOwningProcessor {
    /// Take ownership of an opened window and start driving it.
    ///
    /// One thread per window, never the pump's: windows are not serialised
    /// behind one loop, and a window whose owner has gone quiet must still
    /// answer its window server.
    pub fn start_for_processor_owned_window(processor_owned_window: ProcessorOwnedWindow) -> Self {
        let (initial_width, initial_height) =
            processor_owned_window.current_extent_in_physical_pixels();
        let latest_surface_named_by_the_owning_processor = Arc::new(Mutex::new(None));
        let coalesced_state_for_the_owning_processor = Arc::new(Mutex::new(
            CoalescedProcessorOwnedWindowStateForOwningProcessor {
                current_width_in_physical_pixels: initial_width,
                current_height_in_physical_pixels: initial_height,
                close_requested_by_user: false,
                window_is_closed: false,
            },
        ));
        let frames_composed_and_presented = Arc::new(AtomicU64::new(0));
        let present_loop_keeps_running = Arc::new(AtomicBool::new(true));

        let latest_surface_on_the_loop = Arc::clone(&latest_surface_named_by_the_owning_processor);
        let coalesced_state_on_the_loop = Arc::clone(&coalesced_state_for_the_owning_processor);
        let frames_on_the_loop = Arc::clone(&frames_composed_and_presented);
        let keeps_running_on_the_loop = Arc::clone(&present_loop_keeps_running);

        let present_loop_thread = std::thread::Builder::new()
            .name("processor-owned-window".to_string())
            .spawn(move || {
                drive_the_present_loop_for_one_processor_owned_window(
                    processor_owned_window,
                    &latest_surface_on_the_loop,
                    &coalesced_state_on_the_loop,
                    &frames_on_the_loop,
                    &keeps_running_on_the_loop,
                );
            })
            .expect("failed to spawn a processor-owned window's present thread");

        Self {
            latest_surface_named_by_the_owning_processor,
            coalesced_state_for_the_owning_processor,
            frames_composed_and_presented,
            present_loop_keeps_running,
            present_loop_thread_for_unparking: present_loop_thread.thread().clone(),
            present_loop_thread_awaiting_its_join: Mutex::new(Some(present_loop_thread)),
        }
    }

    /// Name the surface the window shows next, replacing any the loop has not
    /// picked up yet. Never waits on the loop.
    pub fn name_surface_for_the_next_present(
        &self,
        named_surface: SurfaceNamedForTheEnginesPresentLoop,
    ) {
        *self
            .latest_surface_named_by_the_owning_processor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(named_surface);
        self.present_loop_thread_for_unparking.unpark();
    }

    /// Hand the owner its window's coalesced state and clear what a drain
    /// consumes: a close gesture is reported exactly once, while the closed
    /// flag and the extent are the window's current state and stay.
    pub fn drain_coalesced_state_for_the_owning_processor(
        &self,
    ) -> CoalescedProcessorOwnedWindowStateForOwningProcessor {
        let mut coalesced_state = self
            .coalesced_state_for_the_owning_processor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let drained = *coalesced_state;
        coalesced_state.close_requested_by_user = false;
        drained
    }

    /// Whether the engine has closed this window, without consuming anything.
    pub fn window_is_closed(&self) -> bool {
        self.coalesced_state_for_the_owning_processor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .window_is_closed
    }

    /// How many named surfaces this window has composed and presented.
    pub fn frames_composed_and_presented(&self) -> u64 {
        self.frames_composed_and_presented.load(Ordering::Relaxed)
    }

    /// Stop the loop and wait for its thread, which is what closes the
    /// window: the thread owns the pump registration, and dropping that
    /// registration is the close.
    ///
    /// Idempotent — a window the user already closed has no thread left to
    /// join, so an owner's explicit close and processor teardown both land
    /// here and both leave the window reporting closed.
    pub fn close_the_window_and_join_its_present_thread(&self) {
        self.present_loop_keeps_running
            .store(false, Ordering::Release);
        let present_loop_thread = self
            .present_loop_thread_awaiting_its_join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(present_loop_thread) = present_loop_thread else {
            return;
        };
        present_loop_thread.thread().unpark();
        if present_loop_thread.join().is_err() {
            tracing::error!("processor-owned window: the present thread panicked");
        }
    }
}

impl Drop for WindowPresentLoopForOwningProcessor {
    fn drop(&mut self) {
        self.close_the_window_and_join_its_present_thread();
    }
}

/// Resolve, compose and present whatever the owner has named, forever, and
/// close the window when the user asks or the window stops being presentable.
///
/// The close is the engine's to make: the loop cannot wait on a helper's
/// decision, so an owner reacts to a close rather than vetoing one, and a
/// window it never polls still closes.
fn drive_the_present_loop_for_one_processor_owned_window(
    mut processor_owned_window: ProcessorOwnedWindow,
    latest_surface_named_by_the_owning_processor: &Mutex<
        Option<SurfaceNamedForTheEnginesPresentLoop>,
    >,
    coalesced_state_for_the_owning_processor: &Mutex<
        CoalescedProcessorOwnedWindowStateForOwningProcessor,
    >,
    frames_composed_and_presented: &AtomicU64,
    present_loop_keeps_running: &AtomicBool,
) {
    while present_loop_keeps_running.load(Ordering::Acquire) {
        let pending_window_events = match processor_owned_window.apply_pending_window_events() {
            Ok(events) => events,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    window_title = %processor_owned_window.window_title,
                    "processor-owned window: the resize could not be applied — closing the \
                     window, which can no longer present"
                );
                break;
            }
        };
        {
            let (width, height) = processor_owned_window.current_extent_in_physical_pixels();
            let mut coalesced_state = coalesced_state_for_the_owning_processor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            coalesced_state.current_width_in_physical_pixels = width;
            coalesced_state.current_height_in_physical_pixels = height;
            if pending_window_events.close_requested_by_user {
                coalesced_state.close_requested_by_user = true;
            }
        }
        if pending_window_events.close_requested_by_user {
            tracing::info!(
                window_title = %processor_owned_window.window_title,
                "processor-owned window: close requested — closing the window, the pipeline \
                 keeps running"
            );
            break;
        }

        let named_surface = latest_surface_named_by_the_owning_processor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(named_surface) = named_surface else {
            std::thread::park_timeout(PROCESSOR_OWNED_WINDOW_PRESENT_LOOP_IDLE_PARK_INTERVAL);
            continue;
        };

        match processor_owned_window.show_named_surface(named_surface.as_named_surface()) {
            Ok(NamedSurfacePresentationOutcome::ComposedAndPresented) => {
                frames_composed_and_presented.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                window_title = %processor_owned_window.window_title,
                "processor-owned window: present failed"
            ),
        }
    }

    // Dropping the window releases its pump registration, which is what
    // closes it — so the flag is set after, never as a promise ahead of it.
    drop(processor_owned_window);
    coalesced_state_for_the_owning_processor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .window_is_closed = true;
}
