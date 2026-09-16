// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ending the process at once, as the third interrupt and the teardown watchdog
//! both do.

use std::time::Duration;

/// How long the log's drain worker gets to write why the process is going.
const LOG_FLUSH_GRACE_BEFORE_ENDING_THE_PROCESS_AT_ONCE: Duration = Duration::from_millis(100);

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
    crate::core::runtime::kill_every_registered_helper_process_group();
    crate::core::logging::request_a_best_effort_flush();
    std::thread::sleep(LOG_FLUSH_GRACE_BEFORE_ENDING_THE_PROCESS_AT_ONCE);
    // SAFETY: `_exit` takes a scalar status and does not return.
    unsafe { libc::_exit(exit_status) }
}
