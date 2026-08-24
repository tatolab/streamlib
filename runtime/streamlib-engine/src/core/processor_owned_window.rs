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

use std::sync::Arc;

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
    /// The window was brought in line with this frame rather than drawing
    /// it: its compositor was rebuilt for a renegotiated colorspace, or the
    /// window server had already invalidated the swapchain. The next named
    /// surface draws.
    WindowReconciledInsteadOfDrawingThisFrame,
    /// The compositor could not be rebuilt for the attachment format the
    /// renegotiated swapchain picked, so this window cannot draw a frame
    /// carrying this color description — nor any later one, until the
    /// description changes.
    WindowCannotDrawThisFramesColorDescription,
}

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
    /// Declared last so it drops last: the present target's `VkSurfaceKHR`
    /// was minted from this window's raw handle, and the registration above
    /// is otherwise the window's only owner. Holding a clone keeps the
    /// platform window alive past the surface whatever order the fields are
    /// declared in.
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
    /// Errors when the resize's swapchain recreate failed — the window can no
    /// longer present, and what to do about that is the owner's call.
    pub fn apply_pending_window_events(&mut self) -> Result<CoalescedWindowEventsFromEventPump> {
        let events = self.registered_window.drain_window_events_from_event_pump();
        if let Some((width, height)) = events.resized_to_physical_pixels {
            self.present_target
                .recreate(width, height, self.last_applied_color_traits.as_ref())?;
            self.current_width_in_physical_pixels = width;
            self.current_height_in_physical_pixels = height;
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
                        NamedSurfacePresentationOutcome::WindowReconciledInsteadOfDrawingThisFrame,
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
            NamedSurfacePresentationOutcome::WindowReconciledInsteadOfDrawingThisFrame
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

        let new_format = self.present_target.color_format();
        match self.compositor.ensure_attachment_format(new_format) {
            Ok(true) => {
                tracing::info!(
                    ?new_format,
                    window_title = %self.window_title,
                    "processor-owned window: rebuilt compositor for new attachment format"
                );
                ColorspaceRenegotiationOutcome::CompositorRebuiltSoThisFrameIsSkipped
            }
            Ok(false) => ColorspaceRenegotiationOutcome::ThisFrameStillDraws,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    ?new_format,
                    window_title = %self.window_title,
                    "processor-owned window: compositor rebuild failed — this window cannot \
                     draw frames carrying this color description"
                );
                ColorspaceRenegotiationOutcome::CompositorCouldNotBeRebuilt
            }
        }
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
