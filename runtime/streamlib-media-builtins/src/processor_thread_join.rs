// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How a built-in gives up on a thread that will not end.
//!
//! A processor that owns a thread has to hand it back at stop, and a thread
//! parked in a call it does not control cannot be made to return. Waiting
//! forever holds the runtime's whole shutdown chain behind one processor;
//! detaching costs the thread's stack until the process exits, which is the
//! cheaper of the two. Which one a built-in picks is not a per-built-in
//! judgement, so it is stated once.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the wait looks up to see whether the thread has ended. Short
/// against any sensible grace, so the ordinary case — a thread already on its
/// way out — costs a single poll rather than the whole window.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Wait up to `grace` for a thread to end, and detach it if it will not.
///
/// `thread_name_for_the_warning` names the thread in the line a detach emits,
/// because a detached thread is invisible afterwards and the log line is the
/// only record that it happened.
pub fn join_within_grace_or_detach(
    thread_handle: JoinHandle<()>,
    grace: Duration,
    thread_name_for_the_warning: &str,
) {
    let deadline = Instant::now() + grace;
    while !thread_handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(EXIT_POLL_INTERVAL);
    }
    if thread_handle.is_finished() {
        // The join is what surfaces a panic the thread died of; without it a
        // built-in reports a clean stop over a thread that crashed.
        let _ = thread_handle.join();
        return;
    }
    tracing::warn!(
        thread = thread_name_for_the_warning,
        "did not exit within {grace:?}, detaching. Its stack is held until the process exits."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const A_GRACE_LONG_ENOUGH_TO_FINISH_IN: Duration = Duration::from_secs(5);
    const A_GRACE_SHORT_ENOUGH_TO_GIVE_UP_IN: Duration = Duration::from_millis(50);

    #[test]
    fn a_thread_that_ends_is_joined_rather_than_waited_out() {
        let joining_began = Instant::now();
        join_within_grace_or_detach(
            std::thread::spawn(|| {}),
            A_GRACE_LONG_ENOUGH_TO_FINISH_IN,
            "a thread that ends",
        );
        assert!(
            joining_began.elapsed() < A_GRACE_LONG_ENOUGH_TO_FINISH_IN,
            "a thread that had already ended cost {:?} — the wait sat out its window",
            joining_began.elapsed()
        );
    }

    /// The whole reason this is bounded: one processor holding a thread must
    /// not hold the runtime's shutdown chain with it.
    #[test]
    fn a_thread_that_will_not_end_is_given_up_on_rather_than_waited_for() {
        let thread_may_end = Arc::new(AtomicBool::new(false));
        let thread_may_end_in_the_thread = Arc::clone(&thread_may_end);
        let stuck = std::thread::spawn(move || {
            while !thread_may_end_in_the_thread.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let joining_began = Instant::now();
        join_within_grace_or_detach(stuck, A_GRACE_SHORT_ENOUGH_TO_GIVE_UP_IN, "a stuck thread");
        let waited = joining_began.elapsed();

        // Released only after the assertion's subject is over, so the thread is
        // genuinely stuck for the whole window rather than racing the deadline.
        thread_may_end.store(true, Ordering::Release);
        assert!(
            waited >= A_GRACE_SHORT_ENOUGH_TO_GIVE_UP_IN,
            "gave up after {waited:?}, before the grace it promised"
        );
        assert!(
            waited < A_GRACE_LONG_ENOUGH_TO_FINISH_IN,
            "waited {waited:?} on a thread that never ends"
        );
    }
}
