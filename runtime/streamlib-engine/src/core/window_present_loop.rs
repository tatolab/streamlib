// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's one present-loop machinery: per window, resolve a named
//! surface id and compose it onto the present target — acquire, blit,
//! present — paced by vsync, latest-wins.
//!
//! Naming no surface leaves the last one up. The window comes from
//! [`process_wide_window_event_pump`], because winit permits one event loop
//! per process; everything past the raw-window-handle seam — swapchain,
//! acquire, colorspace pick, HDR signalling — is the engine's, and the
//! compositor is internal to this loop rather than a surface any caller
//! names.
//!
//! One machinery, two drivers. A window owner whose code sits in the app
//! process drives it from its own thread, fed however it likes — the
//! built-in display feeds it from an input port. An owner outside the app
//! process cannot: a vsync deadline never crosses a process boundary, so
//! the engine drives the loop on a thread of its own and the owner feeds it
//! by naming published surface ids. Either way the loop is native and never
//! waits on an interpreter, and a window's loop never runs on the pump's
//! thread — windows are not serialised behind one another.

use crate::core::color::{ColorTraits, HdrStaticMetadata};
use crate::core::context::{GpuContextFullAccess, GpuContextLimitedAccess, TextureRegistration};
use crate::core::error::Result;
use crate::core::rhi::pool_slot_key_of_surface_id;
use crate::core::window_event_pump::{
    CoalescedWindowEventsFromEventPump, WindowRegisteredWithEventPump,
    WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
};
use crate::vulkan::rhi::{PresentScalingMode, VulkanPresentCompositor, VulkanPresentTarget};

/// What a window-owning processor asks the engine to run a present loop for.
#[derive(Debug, Clone)]
pub struct WindowPresentLoopRequestFromOwningProcessor {
    /// Window title, owned by the requesting processor.
    pub window_title: String,
    /// Requested initial width in physical pixels.
    pub initial_width_in_physical_pixels: u32,
    /// Requested initial height in physical pixels.
    pub initial_height_in_physical_pixels: u32,
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
    pub hdr_static_metadata_of_frame: Option<&'a HdrStaticMetadata>,
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
    /// The loop spent this call bringing the window in line with the frame
    /// rather than drawing it: a colorspace recreate, a compositor rebuild,
    /// or a swapchain the window server had already invalidated. Each cause
    /// is logged where it happens; the next named surface draws.
    WindowReconciledInsteadOfDrawingThisFrame,
}

/// One window's present loop, driven a named surface at a time.
///
/// Dropping it releases the swapchain and then the window: the present
/// target's surface was minted from the window's raw handle, so the two must
/// go in that order.
pub struct WindowPresentLoopForOwningProcessor {
    gpu_context_for_surface_resolution: GpuContextLimitedAccess,
    window_title: String,
    scaling_mode_for_frame_in_window: PresentScalingMode,
    current_width_in_physical_pixels: u32,
    current_height_in_physical_pixels: u32,
    // Declared ahead of `registered_window`: the present target's surface was
    // minted from that window's raw handle, and fields drop in declaration
    // order, so the surface must go first.
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
}

impl WindowPresentLoopForOwningProcessor {
    /// Mint the window, its present target and its compositor together.
    ///
    /// Takes `GpuContextFullAccess` rather than escalating internally: the
    /// escalate gate serialises rather than reenters, so a caller already
    /// inside `escalate(|full| …)` — every minting escalate op is — would
    /// deadlock against a nested one.
    pub fn open_on_the_process_wide_window_event_pump(
        gpu_context_full_access: &GpuContextFullAccess,
        request: WindowPresentLoopRequestFromOwningProcessor,
    ) -> Result<Self> {
        let registered_window = process_wide_window_event_pump()?
            .request_window_for_owning_processor(WindowRegistrationRequestFromOwningProcessor {
                window_title: request.window_title.clone(),
                initial_width_in_physical_pixels: request.initial_width_in_physical_pixels,
                initial_height_in_physical_pixels: request.initial_height_in_physical_pixels,
            })?;

        let (width, height) = registered_window.current_physical_size();
        let present_target = gpu_context_full_access.create_present_target(
            registered_window.window_shared_with_event_pump().as_ref(),
            width,
            height,
            true,
            None,
        )?;
        let compositor =
            gpu_context_full_access.create_present_compositor(present_target.color_format())?;

        tracing::info!(
            width,
            height,
            window_title = %request.window_title,
            "window present loop: window + present target ready"
        );
        Ok(Self {
            gpu_context_for_surface_resolution: gpu_context_full_access
                .host_inner()
                .limited_access(),
            window_title: request.window_title,
            scaling_mode_for_frame_in_window: request.scaling_mode_for_frame_in_window,
            current_width_in_physical_pixels: width,
            current_height_in_physical_pixels: height,
            present_target,
            compositor,
            registered_window,
            last_applied_color_traits: None,
            last_unresolved_pool_slot_key: None,
            last_failed_recreate_color_traits: None,
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
            self.current_width_in_physical_pixels = width;
            self.current_height_in_physical_pixels = height;
            self.present_target
                .recreate(width, height, self.last_applied_color_traits.as_ref())?;
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
        named_surface: &SurfaceNamedForPresentationOnOwnedWindow<'_>,
    ) -> Result<NamedSurfacePresentationOutcome> {
        if named_surface.color_traits_of_frame != self.last_applied_color_traits
            && self.renegotiate_colorspace_for(named_surface.color_traits_of_frame)
        {
            return Ok(NamedSurfacePresentationOutcome::WindowReconciledInsteadOfDrawingThisFrame);
        }

        // Only meaningful when the picked colorspace is PQ/HLG — gated inside
        // `set_hdr_metadata` — and when the frame carries the sidecar.
        if let Some(metadata) = named_surface.hdr_static_metadata_of_frame
            && let Err(e) = self.present_target.set_hdr_metadata(metadata)
        {
            tracing::warn!(
                error = %e,
                window_title = %self.window_title,
                "window present loop: set_hdr_metadata failed"
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
            tracing::debug!(
                window_title = %self.window_title,
                "window present loop: swapchain out of date at acquire; nothing drawn"
            );
            NamedSurfacePresentationOutcome::WindowReconciledInsteadOfDrawingThisFrame
        })
    }

    /// Bring the swapchain and the compositor in line with a frame whose
    /// color description differs from the last one applied. Answers whether
    /// the reconciliation consumed this frame instead of drawing it.
    ///
    /// First-frame inspection: the present target was constructed with `None`
    /// (legacy SDR pick) and upgrades to whatever the priority walk picks. A
    /// recreate can flip the attachment format (SDR BGRA8 → HDR10
    /// A2B10G10R10), and the compositor's kernel is rebuilt when it does.
    fn renegotiate_colorspace_for(&mut self, color_traits: Option<ColorTraits>) -> bool {
        match self.present_target.recreate(
            self.current_width_in_physical_pixels,
            self.current_height_in_physical_pixels,
            color_traits.as_ref(),
        ) {
            Ok(()) => {
                self.last_applied_color_traits = color_traits;
                self.last_failed_recreate_color_traits = None;
            }
            Err(e) => {
                if self.last_failed_recreate_color_traits != Some(color_traits) {
                    tracing::warn!(
                        error = %e,
                        window_title = %self.window_title,
                        "window present loop: colorspace recreate failed (keeping previous \
                         swapchain; warning once per color description)"
                    );
                    self.last_failed_recreate_color_traits = Some(color_traits);
                }
                return false;
            }
        }

        let new_format = self.present_target.color_format();
        match self.compositor.ensure_attachment_format(new_format) {
            Ok(true) => {
                tracing::info!(
                    ?new_format,
                    window_title = %self.window_title,
                    "window present loop: rebuilt compositor for new attachment format"
                );
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    window_title = %self.window_title,
                    "window present loop: compositor rebuild failed"
                );
                true
            }
        }
    }

    /// Resolve the named id through the engine's blessed order — same-process
    /// texture cache, cross-process DMA-BUF import, pixel-buffer upload —
    /// warning at most once per pool slot on failure.
    fn resolve_named_surface(
        &mut self,
        named_surface: &SurfaceNamedForPresentationOnOwnedWindow<'_>,
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
                    pool_slot_key_of_surface_id(named_surface.surface_id).to_string();
                if self.last_unresolved_pool_slot_key.as_deref()
                    != Some(unresolved_pool_slot_key.as_str())
                {
                    tracing::warn!(
                        surface_id = %named_surface.surface_id,
                        error = %e,
                        window_title = %self.window_title,
                        "window present loop: failed to resolve the named surface \
                         (warning once per surface)"
                    );
                    self.last_unresolved_pool_slot_key = Some(unresolved_pool_slot_key);
                }
                None
            }
        }
    }
}
