// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Result;
use crate::core::execution::ThreadPriority;
use crate::linux::rtkit;

/// Apply thread priority to the current thread.
///
/// Linux gates `SCHED_FIFO` / `SCHED_RR` behind `CAP_SYS_NICE`, which
/// unprivileged desktop processes don't have. We try `rtkit-daemon`
/// first (the freedesktop standard — used by PipeWire / PulseAudio /
/// JACK; brokers the privileged syscall over D-Bus) and only fall
/// back to the direct `pthread_setschedparam` path when rtkit isn't
/// reachable. If both paths fail we log and return cleanly so the
/// thread continues on `SCHED_OTHER`.
///
/// - `RealTime` → SCHED_RR priority 80 (via rtkit), bounded by rtkit's
///   `MaxRealtimePriority` policy when granted directly.
/// - `High` → niceness `-10` (via rtkit), or SCHED_RR priority 50 fallback.
/// - `Normal` → no-op.
pub fn apply_thread_priority(priority: ThreadPriority) -> Result<()> {
    match priority {
        ThreadPriority::RealTime => set_realtime_priority(),
        ThreadPriority::High => set_high_priority(),
        ThreadPriority::Normal => Ok(()),
    }
}

fn set_realtime_priority() -> Result<()> {
    match rtkit::make_current_thread_realtime() {
        Ok(()) => {
            tracing::info!("Applied real-time thread priority via rtkit (SCHED_RR at policy max)");
            return Ok(());
        }
        Err(e) => {
            tracing::warn!(
                "rtkit refused realtime: {e}; falling back to direct pthread_setschedparam"
            );
        }
    }

    set_realtime_priority_direct()
}

fn set_high_priority() -> Result<()> {
    match rtkit::make_current_thread_high_priority() {
        Ok(()) => {
            tracing::info!(
                "Applied high thread priority via rtkit (SCHED_OTHER at policy nice floor)"
            );
            return Ok(());
        }
        Err(e) => {
            tracing::debug!(
                "rtkit refused or unreachable for high priority: {e}; \
                 falling back to direct pthread_setschedparam SCHED_RR"
            );
        }
    }

    set_high_priority_direct()
}

/// Direct `pthread_setschedparam` SCHED_FIFO. Requires `CAP_SYS_NICE`;
/// logs at warn and returns Ok when the syscall fails so the spawned
/// thread continues on its current scheduling class rather than
/// aborting setup.
fn set_realtime_priority_direct() -> Result<()> {
    use libc::{SCHED_FIFO, pthread_self, pthread_setschedparam, sched_param};

    unsafe {
        let thread = pthread_self();
        let mut param: sched_param = std::mem::zeroed();
        param.sched_priority = 80;

        let result = pthread_setschedparam(thread, SCHED_FIFO, &param);
        if result != 0 {
            tracing::warn!(
                "Failed to set SCHED_FIFO real-time thread priority: errno {}. \
                 This requires CAP_SYS_NICE or a running rtkit-daemon.",
                result
            );
            return Ok(());
        }
    }

    tracing::info!("Applied real-time thread priority (SCHED_FIFO, priority 80) — direct syscall");
    Ok(())
}

fn set_high_priority_direct() -> Result<()> {
    use libc::{SCHED_RR, pthread_self, pthread_setschedparam, sched_param};

    unsafe {
        let thread = pthread_self();
        let mut param: sched_param = std::mem::zeroed();
        param.sched_priority = 50;

        let result = pthread_setschedparam(thread, SCHED_RR, &param);
        if result != 0 {
            tracing::warn!(
                "Failed to set SCHED_RR high thread priority: errno {}. \
                 This requires CAP_SYS_NICE or a running rtkit-daemon.",
                result
            );
            return Ok(());
        }
    }

    tracing::info!("Applied high thread priority (SCHED_RR, priority 50) — direct syscall");
    Ok(())
}
