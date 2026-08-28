// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Watching a built-in's worker thread end, for the tests whose claim is that
//! it does.
//!
//! Shared because both audio built-ins make that claim about a device that
//! died, and because the shape matters: a test that joins unconditionally
//! turns the regression it is watching for into a hung suite rather than a red
//! test.

use std::time::{Duration, Instant};

/// Whether a thread ends inside `bound`, without joining one that has not.
pub(crate) fn a_thread_that_finishes_within(
    handle: &std::thread::JoinHandle<()>,
    bound: Duration,
) -> bool {
    let deadline = Instant::now() + bound;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    handle.is_finished()
}
