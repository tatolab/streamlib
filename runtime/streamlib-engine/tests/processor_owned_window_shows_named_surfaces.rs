// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor-owned window driven directly rather than through a processor:
//! name a published surface and it reaches the window; name one that resolves
//! to nothing and the window keeps what it already has.
//!
//! Its own process, because opening one mints the process's one event loop
//! and a Vulkan device alongside it. One test function for the same reason —
//! a second `GpuDevice::new()` in this binary would build a second device.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig
//! only. The end-to-end evidence that N of these coexist is
//! `streamlib-media-builtins`' `two_display_windows_live`.

#![cfg(target_os = "linux")]

use streamlib_engine::core::color::{ColorTraits, PrimariesId, TransferId};
use streamlib_engine::core::context::GpuContext;
use streamlib_engine::core::processor_owned_window::{
    NamedSurfacePresentationOutcome, ProcessorOwnedWindow, ProcessorOwnedWindowRequest,
    SurfaceNamedForPresentationOnOwnedWindow,
};
use streamlib_engine::core::rhi::TextureFormat;
use streamlib_engine::core::window_event_pump::WindowRegistrationRequestFromOwningProcessor;
use streamlib_engine::host_rhi::PresentScalingMode;

const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

/// A surface id that names nothing this process can resolve, in the
/// per-frame `<slot>#<generation>` grammar a retired frame id carries.
const UNRESOLVABLE_SURFACE_ID: &str = "a-surface-this-process-never-saw#7";

/// A cross-process owner's window is driven from a thread the engine owns, so
/// the window has to be movable onto it. Compiled on every target — including
/// CI, where the test below is ignored — so the bound cannot be lost silently
/// and then become a breaking reshape once #1929 needs it.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<ProcessorOwnedWindow>();
};

fn request_for(window_title: &str) -> ProcessorOwnedWindowRequest {
    ProcessorOwnedWindowRequest {
        window_registration_request: WindowRegistrationRequestFromOwningProcessor {
            window_title: window_title.to_string(),
            initial_width_in_physical_pixels: 320,
            initial_height_in_physical_pixels: 240,
        },
        scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server ($DISPLAY / $WAYLAND_DISPLAY) + a GPU. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn a_named_surface_reaches_the_window_and_an_unresolvable_id_leaves_the_last_frame_up() {
    let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
    let gpu_context_limited_access = gpu_context.limited_access();
    let request = request_for("streamlib processor-owned window test");

    // The pump round trip is deliberately outside `escalate`: it touches no
    // GPU, and holding the process-wide gate across it would let a wedged
    // compositor stall every GPU escalation in the process.
    let registered_window =
        ProcessorOwnedWindow::register_window_on_the_process_wide_window_event_pump(&request)
            .expect("the pump mints a window");

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
                        request,
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
    if renegotiating_outcome
        != NamedSurfacePresentationOutcome::WindowCannotDrawThisFramesColorDescription
    {
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

    drop(processor_owned_window);
    drop(source_texture);
}
