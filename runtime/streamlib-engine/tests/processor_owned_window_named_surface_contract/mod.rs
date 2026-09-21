// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The named-surface contract a processor-owned window keeps on every
//! platform: a published surface reaches the window, an id that resolves to
//! nothing leaves the last frame up, a new colour description renegotiates
//! rather than failing, and an untouched window reports no events.

use streamlib_engine::core::color::{ColorTraits, PrimariesId, TransferId};
use streamlib_engine::core::context::GpuContextLimitedAccess;
use streamlib_engine::core::processor_owned_window::{
    NamedSurfacePresentationOutcome, ProcessorOwnedWindow,
    ProcessorOwnedWindowAwaitingItsPresentTarget, ProcessorOwnedWindowRequest,
    SurfaceNamedForPresentationOnOwnedWindow,
};
use streamlib_engine::core::rhi::{Texture, TextureFormat};
use streamlib_engine::core::window_event_pump::WindowRegistrationRequestFromOwningProcessor;
use streamlib_engine::host_rhi::PresentScalingMode;

const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

/// A surface id that names nothing this process can resolve, in the
/// per-frame `<slot>#<generation>` grammar a retired frame id carries.
const UNRESOLVABLE_SURFACE_ID: &str = "a-surface-this-process-never-saw#7";

pub fn request_for(window_title: &str) -> ProcessorOwnedWindowRequest {
    ProcessorOwnedWindowRequest {
        window_registration_request: WindowRegistrationRequestFromOwningProcessor {
            window_title: window_title.to_string(),
            initial_width_in_physical_pixels: 320,
            initial_height_in_physical_pixels: 240,
        },
        scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
    }
}

/// Mint `registered_window`'s present target beside a published surface and
/// hold the window to the contract. Hands both back, so the caller decides
/// when — and on which thread — they go.
pub fn mint_and_hold_a_window_to_the_named_surface_contract(
    gpu_context_limited_access: &GpuContextLimitedAccess,
    registered_window: ProcessorOwnedWindowAwaitingItsPresentTarget,
) -> (ProcessorOwnedWindow, Texture) {
    // Registered outside `escalate`, minted inside it — the ordering the
    // carrier type exists to keep.
    let (published_surface_id, source_texture, mut processor_owned_window) =
        gpu_context_limited_access
            .escalate(|gpu_context_full_access| {
                let (published_surface_id, source_texture) = gpu_context_full_access
                    .acquire_output_texture(
                        SOURCE_EXTENT_IN_PIXELS,
                        SOURCE_EXTENT_IN_PIXELS,
                        TextureFormat::Bgra8Unorm,
                    )?;
                let processor_owned_window =
                    ProcessorOwnedWindow::open_present_target_for_registered_window(
                        gpu_context_full_access,
                        registered_window,
                    )?;
                Ok((published_surface_id, source_texture, processor_owned_window))
            })
            .expect("the present target and compositor mint under one escalate");

    let (width, height) = processor_owned_window.current_extent_in_physical_pixels();
    assert!(
        width > 0 && height > 0,
        "the window reports a legal swapchain extent, got {width}x{height}"
    );

    let named_surface = SurfaceNamedForPresentationOnOwnedWindow {
        surface_id: &published_surface_id,
        source_width_in_pixels: SOURCE_EXTENT_IN_PIXELS,
        source_height_in_pixels: SOURCE_EXTENT_IN_PIXELS,
        producer_published_texture_layout: None,
        color_traits_of_frame: None,
        hdr_static_metadata_of_frame: None,
    };
    assert_eq!(
        processor_owned_window
            .show_named_surface(named_surface)
            .expect("a resolvable id composes and presents"),
        NamedSurfacePresentationOutcome::ComposedAndPresented,
        "naming a published surface must reach the window"
    );

    // Latest-wins, and the surface-id lifetime contract: an id that resolves
    // to nothing is never someone else's pixels and never takes the window
    // down — it simply keeps the frame it already has.
    assert_eq!(
        processor_owned_window
            .show_named_surface(SurfaceNamedForPresentationOnOwnedWindow {
                surface_id: UNRESOLVABLE_SURFACE_ID,
                ..named_surface
            })
            .expect("an id that resolves to nothing is an outcome, not an error"),
        NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve,
        "an unresolvable id must be reported rather than drawn or raised"
    );

    assert_eq!(
        processor_owned_window
            .show_named_surface(named_surface)
            .expect("the window still presents after an unresolvable id"),
        NamedSurfacePresentationOutcome::ComposedAndPresented,
        "one unresolvable id must not wedge the window for every later frame"
    );

    // Colorspace renegotiation. Whether the swapchain's attachment format
    // actually flips is the display's call, so the outcome of the renegotiating
    // frame is not fixed — what is fixed is that describing a frame never
    // fails, and that the window is drawing again on the frame after it.
    let named_hdr_surface = SurfaceNamedForPresentationOnOwnedWindow {
        color_traits_of_frame: Some(ColorTraits {
            primaries: Some(PrimariesId::Bt2020),
            transfer: Some(TransferId::Pq),
        }),
        ..named_surface
    };
    let renegotiating_outcome = processor_owned_window
        .show_named_surface(named_hdr_surface)
        .expect("a frame carrying a new color description renegotiates rather than failing");
    assert_ne!(
        renegotiating_outcome,
        NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve,
        "renegotiation must not lose the surface the frame named"
    );
    // Only the rebuild outcome promises the next frame draws. The other two
    // reachable here say the opposite in their own docs — a window that cannot
    // take the description, and a swapchain waiting on a resize.
    if matches!(
        renegotiating_outcome,
        NamedSurfacePresentationOutcome::ComposedAndPresented
            | NamedSurfacePresentationOutcome::CompositorRebuiltForThisFramesColorDescription
    ) {
        assert_eq!(
            processor_owned_window
                .show_named_surface(named_hdr_surface)
                .expect("the frame after a renegotiation presents"),
            NamedSurfacePresentationOutcome::ComposedAndPresented,
            "a renegotiation that reported a rebuild must leave the next frame drawable"
        );
    }

    // Polling is optional and benign: an untouched window reports no resize
    // and no close, and asking costs nothing.
    let events = processor_owned_window
        .apply_pending_window_events()
        .expect("draining an untouched window's events");
    assert!(
        !events.close_requested_by_user,
        "nobody closed this window, so no close-request may be reported"
    );

    (processor_owned_window, source_texture)
}
