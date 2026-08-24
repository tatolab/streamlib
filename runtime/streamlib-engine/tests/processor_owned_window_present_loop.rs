// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The present loop the engine runs for a window whose owner's code cannot
//! sit in the app process: it shows what the owner names, keeps the last
//! frame up when the owner names nothing, and never makes the owner wait on
//! a vsync.
//!
//! Its own process, because opening a window mints the process's one event
//! loop and a Vulkan device alongside it. One test function for the same
//! reason. The wire that reaches this loop from a helper process is
//! `processor_owned_window_over_the_escalate_wire`.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig
//! only.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use streamlib_engine::core::color::{ColorTraits, PrimariesId, TransferId};
use streamlib_engine::core::context::GpuContext;
use streamlib_engine::core::processor_owned_window::{
    ProcessorOwnedWindow, ProcessorOwnedWindowAwaitingItsPresentTarget,
    ProcessorOwnedWindowRequest, SurfaceNamedForTheEnginesPresentLoop,
    WindowPresentLoopForOwningProcessor,
};
use streamlib_engine::core::rhi::TextureFormat;
use streamlib_engine::core::window_event_pump::{
    WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
};
use streamlib_engine::host_rhi::PresentScalingMode;

const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

/// How long a named surface may take to reach the window before the loop is
/// considered wedged. Generous — one vsync is the honest budget.
const HOW_LONG_A_NAMED_SURFACE_MAY_TAKE_TO_REACH_THE_WINDOW: Duration = Duration::from_secs(5);

/// Long enough for the loop to show anything still owed at a 60Hz vsync, and
/// far too short for a loop that queued frames instead of keeping only the
/// latest to work through the backlog named below.
const HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE: Duration = Duration::from_millis(300);

/// Named in one tight burst. At vsync this is five seconds of presenting, so
/// a loop that still has frames to show after two quiet windows is queueing
/// rather than keeping only the latest.
const IDS_NAMED_IN_ONE_BURST: usize = 300;

/// A surface id that names nothing this process can resolve, in the
/// per-frame `<slot>#<generation>` grammar a retired frame id carries.
const UNRESOLVABLE_SURFACE_ID: &str = "a-surface-this-process-never-saw#11";

fn named_surface(surface_id: &str) -> SurfaceNamedForTheEnginesPresentLoop {
    SurfaceNamedForTheEnginesPresentLoop {
        surface_id: surface_id.to_string(),
        source_width_in_pixels: SOURCE_EXTENT_IN_PIXELS,
        source_height_in_pixels: SOURCE_EXTENT_IN_PIXELS,
        producer_published_texture_layout: None,
        color_traits_of_frame: None,
        hdr_static_metadata_of_frame: None,
    }
}

/// The same frame, described. The escalate wire carries no colour today, so
/// this arm is what keeps the loop's colour path honest until it does — the
/// seam renegotiates the swapchain on a description change, and a window that
/// could not take one would stop presenting rather than fall back.
fn named_surface_carrying_a_colour_description(
    surface_id: &str,
) -> SurfaceNamedForTheEnginesPresentLoop {
    SurfaceNamedForTheEnginesPresentLoop {
        color_traits_of_frame: Some(ColorTraits {
            primaries: Some(PrimariesId::Bt2020),
            transfer: Some(TransferId::Pq),
        }),
        ..named_surface(surface_id)
    }
}

fn wait_until_the_window_has_presented(
    present_loop: &WindowPresentLoopForOwningProcessor,
    at_least_this_many_frames: u64,
) -> u64 {
    let deadline = Instant::now() + HOW_LONG_A_NAMED_SURFACE_MAY_TAKE_TO_REACH_THE_WINDOW;
    while Instant::now() < deadline {
        let presented = present_loop.frames_composed_and_presented();
        if presented >= at_least_this_many_frames {
            return presented;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "the window presented {} frames, expected at least {at_least_this_many_frames} within \
         {HOW_LONG_A_NAMED_SURFACE_MAY_TAKE_TO_REACH_THE_WINDOW:?}",
        present_loop.frames_composed_and_presented()
    );
}

fn wait_until_the_pump_routes_to_exactly(expected_window_count: usize) {
    let pump = process_wide_window_event_pump().expect("the pump minted this test's window");
    // The pump's count lags a registration's drop by the round trip its
    // deregistration takes through the event loop, so this polls.
    let deadline = Instant::now() + HOW_LONG_A_NAMED_SURFACE_MAY_TAKE_TO_REACH_THE_WINDOW;
    while Instant::now() < deadline {
        if pump.registered_window_count() == expected_window_count {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "the pump routes to {} windows, expected {expected_window_count}",
        pump.registered_window_count()
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server ($DISPLAY / $WAYLAND_DISPLAY) + a GPU. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn the_engine_run_loop_shows_what_its_owner_names_without_ever_pacing_the_owner() {
    let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
    let gpu_context_limited_access = gpu_context.limited_access();

    let registered_window =
        ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
            ProcessorOwnedWindowRequest {
                window_registration_request: WindowRegistrationRequestFromOwningProcessor {
                    window_title: "streamlib processor-owned window present loop".to_string(),
                    initial_width_in_physical_pixels: 320,
                    initial_height_in_physical_pixels: 240,
                },
                scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
            },
        )
        .expect("the pump mints a window");

    let (published_surface_id, source_texture, processor_owned_window) = gpu_context_limited_access
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

    let present_loop = WindowPresentLoopForOwningProcessor::start_for_processor_owned_window(
        processor_owned_window,
    )
    .expect("the present thread spawns");
    wait_until_the_pump_routes_to_exactly(1);

    let coalesced_state = present_loop.drain_coalesced_state_for_the_owning_processor();
    assert!(
        coalesced_state.current_width_in_physical_pixels > 0
            && coalesced_state.current_height_in_physical_pixels > 0,
        "the owner is told a legal extent, got {}x{}",
        coalesced_state.current_width_in_physical_pixels,
        coalesced_state.current_height_in_physical_pixels
    );
    assert!(
        !coalesced_state.close_requested_by_user && !coalesced_state.window_is_closed,
        "nobody has closed this window, so neither may be reported"
    );

    present_loop.name_surface_for_the_next_present(named_surface(&published_surface_id));
    let presented_after_the_first_named_surface =
        wait_until_the_window_has_presented(&present_loop, 1);

    // Naming nothing leaves the last frame up: the loop has nothing to show,
    // so it shows nothing again rather than re-presenting what it already has.
    std::thread::sleep(HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE);
    assert_eq!(
        present_loop.frames_composed_and_presented(),
        presented_after_the_first_named_surface,
        "a quiet owner must leave the window's last frame up, not drive fresh presents"
    );
    assert!(
        !present_loop.window_is_closed(),
        "a quiet owner must not lose its window"
    );

    // The surface-id lifetime contract at the loop: an id that resolves to
    // nothing is never someone else's pixels, never takes the window down,
    // and never wedges it for the frames that follow.
    present_loop.name_surface_for_the_next_present(named_surface(UNRESOLVABLE_SURFACE_ID));
    std::thread::sleep(HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE);
    assert_eq!(
        present_loop.frames_composed_and_presented(),
        presented_after_the_first_named_surface,
        "an id that resolves to nothing must not count as a frame the window showed"
    );
    assert!(
        !present_loop.window_is_closed(),
        "an id that resolves to nothing must not close the window"
    );

    // The owner's pace never meets the window's: naming a burst returns at
    // memory speed, and what the window did with the burst is its own affair.
    let burst_started = Instant::now();
    for _ in 0..IDS_NAMED_IN_ONE_BURST {
        present_loop.name_surface_for_the_next_present(named_surface(&published_surface_id));
    }
    let naming_the_burst_took = burst_started.elapsed();
    assert!(
        naming_the_burst_took < Duration::from_millis(500),
        "naming {IDS_NAMED_IN_ONE_BURST} surfaces took {naming_the_burst_took:?} — the owner is \
         being paced by the window, which is the vsync deadline crossing a hop it must never cross"
    );

    // Panics unless the burst reached the window at all.
    wait_until_the_window_has_presented(&present_loop, presented_after_the_first_named_surface + 1);

    // Latest-wins, from the outside: the loop keeps only the newest id, so a
    // burst leaves no backlog. A loop that queued them would still be
    // presenting for seconds after this.
    std::thread::sleep(HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE);
    let presented_once_the_burst_settled = present_loop.frames_composed_and_presented();
    std::thread::sleep(HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE);
    assert_eq!(
        present_loop.frames_composed_and_presented(),
        presented_once_the_burst_settled,
        "the window is still working through a backlog {IDS_NAMED_IN_ONE_BURST} ids deep, so the \
         loop queued what it was named instead of keeping only the latest"
    );
    assert!(
        presented_once_the_burst_settled < IDS_NAMED_IN_ONE_BURST as u64,
        "the window presented {presented_once_the_burst_settled} of {IDS_NAMED_IN_ONE_BURST} \
         named ids — latest-wins means most of a burst is dropped, never shown late"
    );
    // A described frame renegotiates rather than failing, and the window is
    // presenting again on the frame after it. Whether the swapchain's format
    // actually flips is the window server's call, so what is asserted is that
    // naming a colour never wedges the loop.
    present_loop.name_surface_for_the_next_present(named_surface_carrying_a_colour_description(
        &published_surface_id,
    ));
    std::thread::sleep(HOW_LONG_A_QUIET_OWNER_LEAVES_THE_LOOP_ALONE);
    let presented_before_the_undescribed_frame = present_loop.frames_composed_and_presented();
    present_loop.name_surface_for_the_next_present(named_surface(&published_surface_id));
    wait_until_the_window_has_presented(&present_loop, presented_before_the_undescribed_frame + 1);
    assert!(
        !present_loop.window_is_closed(),
        "a frame carrying a colour description must not cost the owner its window"
    );

    // Closing joins the present thread, and dropping its pump registration is
    // what closes the window — the release a processor's teardown owes.
    assert!(
        present_loop.close_the_window_and_join_its_present_thread(),
        "the close must answer that the window actually closed"
    );
    assert!(
        present_loop.window_is_closed(),
        "a closed window must report itself closed"
    );
    wait_until_the_pump_routes_to_exactly(0);

    // Idempotent: teardown closes every window the owner still holds, and a
    // window the owner already closed has no thread left to wait for.
    assert!(present_loop.close_the_window_and_join_its_present_thread());
    assert!(present_loop.window_is_closed());

    drop(present_loop);
    drop(source_texture);
}
