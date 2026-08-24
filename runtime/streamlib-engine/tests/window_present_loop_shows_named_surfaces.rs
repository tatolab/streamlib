// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's present loop, driven directly rather than through a
//! processor: name a published surface and it reaches the window; name one
//! that resolves to nothing and the window keeps what it already has.
//!
//! Its own process, because the loop mints the process's one event loop and
//! a Vulkan device alongside it. One test function for the same reason —
//! a second `GpuDevice::new()` in this binary would build a second device.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig
//! only. The end-to-end evidence that N of these coexist is
//! `streamlib-media-builtins`' `two_display_windows_live`.

#![cfg(target_os = "linux")]

use streamlib_engine::core::context::GpuContext;
use streamlib_engine::core::rhi::TextureFormat;
use streamlib_engine::core::window_present_loop::{
    NamedSurfacePresentationOutcome, SurfaceNamedForPresentationOnOwnedWindow,
    WindowPresentLoopForOwningProcessor, WindowPresentLoopRequestFromOwningProcessor,
};
use streamlib_engine::host_rhi::PresentScalingMode;

const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

/// A surface id that names nothing this process can resolve, in the
/// per-frame `<slot>#<generation>` grammar a retired frame id carries.
const UNRESOLVABLE_SURFACE_ID: &str = "a-surface-this-process-never-saw#7";

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server ($DISPLAY / $WAYLAND_DISPLAY) + a GPU. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn a_named_surface_reaches_the_window_and_an_unresolvable_id_leaves_the_last_frame_up() {
    let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
    let gpu_context_limited_access = gpu_context.limited_access();

    // Both minting steps sit inside one escalate: the loop takes FullAccess
    // because the gate serialises rather than reenters, so it cannot take one
    // for itself.
    let (published_surface_id, source_texture, mut window_present_loop) =
        gpu_context_limited_access
            .escalate(|gpu_context_full_access| {
                let (published_surface_id, source_texture) = gpu_context_full_access
                    .acquire_output_texture(
                        SOURCE_EXTENT_IN_PIXELS,
                        SOURCE_EXTENT_IN_PIXELS,
                        TextureFormat::Bgra8Unorm,
                    )?;
                let window_present_loop =
                    WindowPresentLoopForOwningProcessor::open_on_the_process_wide_window_event_pump(
                        gpu_context_full_access,
                        WindowPresentLoopRequestFromOwningProcessor {
                            window_title: "streamlib present-loop seam test".to_string(),
                            initial_width_in_physical_pixels: 320,
                            initial_height_in_physical_pixels: 240,
                            scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
                        },
                    )?;
                Ok((published_surface_id, source_texture, window_present_loop))
            })
            .expect("one request mints the window, its present target and its compositor");

    let (width, height) = window_present_loop.current_extent_in_physical_pixels();
    assert!(
        width > 0 && height > 0,
        "the loop reports a legal swapchain extent, got {width}x{height}"
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
        window_present_loop
            .show_named_surface(&named_surface)
            .expect("a resolvable id composes and presents"),
        NamedSurfacePresentationOutcome::ComposedAndPresented,
        "naming a published surface must reach the window"
    );

    // Latest-wins, and the surface-id lifetime contract: an id that resolves
    // to nothing is never someone else's pixels and never takes the loop
    // down — the window simply keeps the frame it already has.
    let named_unresolvable_surface = SurfaceNamedForPresentationOnOwnedWindow {
        surface_id: UNRESOLVABLE_SURFACE_ID,
        ..named_surface
    };
    assert_eq!(
        window_present_loop
            .show_named_surface(&named_unresolvable_surface)
            .expect("an id that resolves to nothing is an outcome, not an error"),
        NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve,
        "an unresolvable id must be reported rather than drawn or raised"
    );

    assert_eq!(
        window_present_loop
            .show_named_surface(&named_surface)
            .expect("the loop still presents after an unresolvable id"),
        NamedSurfacePresentationOutcome::ComposedAndPresented,
        "one unresolvable id must not wedge the window for every later frame"
    );

    // Polling is optional and benign: an untouched window reports no resize
    // and no close, and asking costs nothing.
    let events = window_present_loop
        .apply_pending_window_events()
        .expect("draining an untouched window's events");
    assert!(
        !events.close_requested_by_user,
        "nobody closed this window, so no close-request may be reported"
    );

    drop(window_present_loop);
    drop(source_texture);
}
