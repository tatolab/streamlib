// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::execution::ThreadPriority;
use crate::core::Result;

/// Apply thread priority to the current thread based on the specified priority level.
///
/// This should be called from within a spawned thread to set its scheduling priority.
/// - `RealTime`: Uses POSIX `SCHED_FIFO` with priority 80 for strict latency guarantees
/// - `High`: Uses POSIX `SCHED_RR` with priority 50 for elevated priority
/// - `Normal`: No changes (default OS scheduling)
///
/// Note: `SCHED_FIFO` and `SCHED_RR` require `CAP_SYS_NICE` or root privileges.
/// If unavailable, a warning is logged and the thread continues with normal priority.
pub fn apply_thread_priority(priority: ThreadPriority) -> Result<()> {
    match priority {
        ThreadPriority::RealTime => set_realtime_priority(),
        ThreadPriority::High => set_high_priority(),
        ThreadPriority::Normal => Ok(()), // No-op for normal priority
    }
}

fn set_realtime_priority() -> Result<()> {
    use libc::{pthread_self, pthread_setschedparam, sched_param, SCHED_FIFO};

    unsafe {
        let thread = pthread_self();
        let mut param: sched_param = std::mem::zeroed();

        // SCHED_FIFO priority 80 — high within the 1-99 range for real-time audio
        param.sched_priority = 80;

        let result = pthread_setschedparam(thread, SCHED_FIFO, &param);

        if result != 0 {
            // EPERM (1) is expected when CAP_SYS_NICE is not available.
            // Log a warning but don't fail — the thread will run with normal priority.
            tracing::warn!(
                "Failed to set SCHED_FIFO real-time thread priority: errno {}. \
                 This requires CAP_SYS_NICE or root privileges.",
                result
            );
            return Ok(());
        }
    }

    tracing::info!("Applied real-time thread priority (SCHED_FIFO, priority 80)");
    Ok(())
}

fn set_high_priority() -> Result<()> {
    use libc::{pthread_self, pthread_setschedparam, sched_param, SCHED_RR};

    unsafe {
        let thread = pthread_self();
        let mut param: sched_param = std::mem::zeroed();

        // SCHED_RR priority 50 — middle of the 1-99 range
        param.sched_priority = 50;

        let result = pthread_setschedparam(thread, SCHED_RR, &param);

        if result != 0 {
            // May fail without CAP_SYS_NICE — log warning but continue
            tracing::warn!(
                "Failed to set SCHED_RR high thread priority: errno {}. \
                 This may require elevated privileges.",
                result
            );
            return Ok(());
        }
    }

    tracing::info!("Applied high thread priority (SCHED_RR, priority 50)");
    Ok(())
}
