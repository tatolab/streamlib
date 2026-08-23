// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The process-wide window event pump serves every caller from one event loop.
//!
//! winit permits one `EventLoop` per process and refuses the second for the
//! process's whole life, so the regression this file guards is not "a window
//! failed" but "the second caller was answered differently from the first".
//! Its own process, because building that one loop is irreversible.

#![cfg(target_os = "linux")]

use streamlib_engine::core::window_event_pump::{
    WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
};

fn request_for(window_title: &str) -> WindowRegistrationRequestFromOwningProcessor {
    WindowRegistrationRequestFromOwningProcessor {
        window_title: window_title.to_string(),
        initial_width_in_physical_pixels: 320,
        initial_height_in_physical_pixels: 240,
    }
}

/// Runs everywhere, including headless CI: whether or not a display server
/// exists, the second caller must get the same answer as the first. A pump
/// that could not start says so identically forever instead of letting a later
/// caller burn a second event-loop attempt.
#[test]
fn every_caller_is_answered_by_the_same_pump() {
    let first = process_wide_window_event_pump();
    let second = process_wide_window_event_pump();

    match (first, second) {
        (Ok(first), Ok(second)) => assert!(
            std::ptr::eq(first, second),
            "both callers must reach one pump, not one each"
        ),
        (Err(first), Err(second)) => {
            assert_eq!(
                first.to_string(),
                second.to_string(),
                "a pump that cannot start refuses identically; the second caller must not \
                 get a different failure"
            );
            assert!(
                !first.to_string().contains("recreated"),
                "a second attempt at the process's one event loop was made: {first}"
            );
        }
        (first, second) => panic!(
            "the two callers disagreed about whether a pump exists: {:?} vs {:?}",
            first.map(|_| "pump"),
            second.map(|_| "pump")
        ),
    }
}

/// Needs a display server, so it is rig-only under the hardware tier. This is
/// the ticket's named check: a second window registration is accepted rather
/// than degraded.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "needs a display server ($DISPLAY / $WAYLAND_DISPLAY) — set --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn two_windows_register_at_once_and_each_is_addressed_alone() {
    let event_pump = process_wide_window_event_pump()
        .expect("a display server is required for this tier-2 test");

    let first_window = event_pump
        .request_window_for_owning_processor(request_for("streamlib pump test — first"))
        .expect("the first window registration");
    let second_window = event_pump
        .request_window_for_owning_processor(request_for("streamlib pump test — second"))
        .expect(
            "the second window registration — a refusal here is the one-event-loop-per-process \
             regression this pump exists to remove",
        );

    assert_ne!(
        first_window.window_shared_with_event_pump().id(),
        second_window.window_shared_with_event_pump().id(),
        "two live windows, not one handed out twice"
    );
    for (label, window) in [("first", &first_window), ("second", &second_window)] {
        let (width, height) = window.current_physical_size();
        assert!(
            width > 0 && height > 0,
            "the {label} window reports a legal swapchain extent, got {width}x{height}"
        );
    }

    // Dropping one window leaves the other registered and serviceable — the
    // failure mode where deregistering tears down the shared loop.
    drop(first_window);
    let (width, height) = second_window.current_physical_size();
    assert!(
        width > 0 && height > 0,
        "the surviving window still answers after its neighbour deregistered"
    );
    assert!(
        process_wide_window_event_pump().is_ok(),
        "the pump outlives any one window's registration"
    );
}
