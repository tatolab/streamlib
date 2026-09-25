// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ending the process at once, as the third interrupt and the teardown watchdog
//! both do.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the log's drain worker gets to write why the process is going.
const LOG_FLUSH_GRACE_BEFORE_ENDING_THE_PROCESS_AT_ONCE: Duration = Duration::from_millis(100);

/// Set before the first helper group is killed, and never cleared: the process
/// has no future past the `_exit` that follows.
static THE_PROCESS_IS_ENDING_AT_ONCE: AtomicBool = AtomicBool::new(false);

/// Never return while another thread is ending the process at once.
///
/// Killing the helper groups is what unblocks a run loop's teardown, so a run
/// loop that returned during the log's flush grace would let its caller exit the
/// process first, with whatever status it reached.
pub(crate) fn park_forever_if_the_process_is_ending_at_once() {
    if THE_PROCESS_IS_ENDING_AT_ONCE.load(Ordering::SeqCst) {
        park_forever();
    }
}

/// Block this thread until the process ends.
pub(crate) fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// Kill every registered helper process group, give the log a moment, and
/// `_exit` with `exit_status`.
///
/// The groups go first because the kernel's parent-death signal reaches each
/// helper but never a process a helper forked. `_exit`, never `exit`: an
/// `atexit` hook or a destructor would run the very teardown that was just
/// interrupted, or has hung.
pub(crate) fn kill_every_helper_process_group_and_end_the_process_at_once(
    exit_status: libc::c_int,
) -> ! {
    THE_PROCESS_IS_ENDING_AT_ONCE.store(true, Ordering::SeqCst);
    crate::core::runtime::kill_every_registered_helper_process_group();
    crate::core::logging::request_a_best_effort_flush();
    std::thread::sleep(LOG_FLUSH_GRACE_BEFORE_ENDING_THE_PROCESS_AT_ONCE);
    // SAFETY: `_exit` takes a scalar status and does not return.
    unsafe { libc::_exit(exit_status) }
}
