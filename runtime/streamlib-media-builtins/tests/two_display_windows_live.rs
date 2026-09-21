// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One source fanned out to two `DisplayWindow` instances in one process.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig only.
//! It is also the harness `/verify-live` drives to capture both windows —
//! `STREAMLIB_TWO_WINDOW_HARNESS_SECONDS` holds the graph up long enough to
//! photograph.
//!
//! The assertion is made against the window server rather than the graph,
//! because the regression it guards was invisible to the graph: before the
//! shared pump, the second display logged `EventLoop can't be recreated` and
//! its render thread exited, while the graph went on reporting two healthy
//! processors.

#![cfg(target_os = "linux")]

mod two_display_windows_harness;

use std::process::Command;
use std::time::{Duration, Instant};

use streamlib::sdk::App;

use two_display_windows_harness::{
    FIRST_WINDOW_TITLE, SECOND_WINDOW_TITLE, add_one_source_fanned_out_to_two_display_windows,
    harness_duration,
};

/// How many windows the window server currently shows under `title`.
///
/// `--onlyvisible` because a bare search also returns unmapped windows, and a
/// window that exists but was never mapped is exactly the failure this test is
/// meant to catch.
fn windows_on_screen_titled(title: &str) -> usize {
    let output = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--name", title])
        .output()
        .expect("xdotool must be installed to assert against the window server");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server + GPU. Run with --features streamlib-media-builtins/hardware-tests; the workspace sweep in docs/testing-hardware.md names it explicitly, because streamlib/hardware-tests does not reach this crate"
)]
#[test]
fn one_source_feeds_two_display_windows_at_once() {
    let app = App::new().expect("runtime");
    add_one_source_fanned_out_to_two_display_windows(&app);

    app.runner().start().expect("the graph starts");

    // Both windows must be mapped before the harness window closes. Poll
    // rather than sleep a fixed warm-up: swapchain creation on a cold GPU is
    // the slow step and it varies.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut first_seen = 0;
    let mut second_seen = 0;
    while Instant::now() < deadline {
        first_seen = windows_on_screen_titled(FIRST_WINDOW_TITLE);
        second_seen = windows_on_screen_titled(SECOND_WINDOW_TITLE);
        if first_seen > 0 && second_seen > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // Hold the graph up so a capture can be taken against live windows.
    std::thread::sleep(harness_duration());
    let stop_outcome = app.runner().stop();

    assert_eq!(
        (first_seen, second_seen),
        (1, 1),
        "both displays must own a live window at the same time; the window server showed \
         {first_seen} titled '{FIRST_WINDOW_TITLE}' and {second_seen} titled \
         '{SECOND_WINDOW_TITLE}'"
    );
    stop_outcome.expect("the graph stops cleanly with two windows open");
}
