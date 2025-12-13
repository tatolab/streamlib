// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::delegates::ThreadPriority;
use crate::core::{Result, StreamError};

/// Apply thread priority to the current thread based on the specified priority level.
///
/// This should be called from within a spawned thread to set its scheduling priority.
/// - `RealTime`: Uses Mach time-constraint policy for strict latency guarantees
/// - `High`: Uses POSIX SCHED_RR with elevated priority
/// - `Normal`: No changes (default OS scheduling)
pub fn apply_thread_priority(priority: ThreadPriority) -> Result<()> {
    match priority {
        ThreadPriority::RealTime => set_realtime_priority(),
        ThreadPriority::High => set_high_priority(),
        ThreadPriority::Normal => Ok(()), // No-op for normal priority
    }
}

#[cfg(target_os = "macos")]
fn set_realtime_priority() -> Result<()> {
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::thread_policy::{
        thread_time_constraint_policy_data_t, THREAD_TIME_CONSTRAINT_POLICY,
    };

    extern "C" {
        fn mach_thread_self() -> u32;
        fn thread_policy_set(
            thread: u32,
            flavor: u32,
            policy_info: *const i32,
            policy_info_count: u32,
        ) -> i32;
    }

    // Real-time audio constraints (10ms period, tight constraints)
    // These values work well for audio processing
    let period_ns = 10_000_000u64; // 10ms in nanoseconds
    let computation_ns = 5_000_000u64; // 5ms computation time
    let constraint_ns = 7_000_000u64; // 7ms constraint (must finish within this)

    // Convert nanoseconds to Mach absolute time units
    // On Apple Silicon, the timebase is 1:1 with nanoseconds
    let mut timebase_info = mach2::mach_time::mach_timebase_info_data_t { numer: 0, denom: 0 };

    unsafe {
        mach2::mach_time::mach_timebase_info(&mut timebase_info as *mut _);

        let period = (period_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let computation =
            (computation_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let constraint = (constraint_ns * timebase_info.denom as u64) / timebase_info.numer as u64;

        let policy = thread_time_constraint_policy_data_t {
            period: period as u32,
            computation: computation as u32,
            constraint: constraint as u32,
            preemptible: 1, // Allow preemption (safer)
        };

        let result = thread_policy_set(
            mach_thread_self(),
            THREAD_TIME_CONSTRAINT_POLICY,
            &policy as *const _ as *const i32,
            (std::mem::size_of::<thread_time_constraint_policy_data_t>() / 4) as u32,
        );

        if result != KERN_SUCCESS {
            return Err(StreamError::Runtime(format!(
                "Failed to set real-time thread priority: mach error {}",
                result
            )));
        }
    }

    tracing::info!(
        "Applied real-time thread priority (10ms period, 5ms computation, 7ms constraint)"
    );
    Ok(())
}

#[cfg(target_os = "ios")]
fn set_realtime_priority() -> Result<()> {
    // iOS uses same Mach APIs as macOS
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::thread_policy::{
        thread_time_constraint_policy_data_t, THREAD_TIME_CONSTRAINT_POLICY,
    };

    extern "C" {
        fn mach_thread_self() -> u32;
        fn thread_policy_set(
            thread: u32,
            flavor: u32,
            policy_info: *const i32,
            policy_info_count: u32,
        ) -> i32;
    }

    let period_ns = 10_000_000u64;
    let computation_ns = 5_000_000u64;
    let constraint_ns = 7_000_000u64;

    let mut timebase_info = mach2::mach_time::mach_timebase_info_data_t { numer: 0, denom: 0 };

    unsafe {
        mach2::mach_time::mach_timebase_info(&mut timebase_info as *mut _);

        let period = (period_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let computation =
            (computation_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let constraint = (constraint_ns * timebase_info.denom as u64) / timebase_info.numer as u64;

        let policy = thread_time_constraint_policy_data_t {
            period: period as u32,
            computation: computation as u32,
            constraint: constraint as u32,
            preemptible: 1,
        };

        let result = thread_policy_set(
            mach_thread_self(),
            THREAD_TIME_CONSTRAINT_POLICY,
            &policy as *const _ as *const i32,
            (std::mem::size_of::<thread_time_constraint_policy_data_t>() / 4) as u32,
        );

        if result != KERN_SUCCESS {
            return Err(StreamError::Runtime(format!(
                "Failed to set real-time thread priority: mach error {}",
                result
            )));
        }
    }

    tracing::info!("Applied real-time thread priority (iOS)");
    Ok(())
}

fn set_high_priority() -> Result<()> {
    // Use POSIX thread priority for High priority (not real-time)
    // This gives elevated priority without real-time constraints
    use libc::{pthread_self, pthread_setschedparam, sched_param, SCHED_RR};

    unsafe {
        let thread = pthread_self();
        let mut param: sched_param = std::mem::zeroed();

        // Set priority to 50 (middle of the range for SCHED_RR)
        // Range is typically 1-99, with higher being more priority
        param.sched_priority = 50;

        let result = pthread_setschedparam(thread, SCHED_RR, &param);

        if result != 0 {
            // Note: This may fail if not running with appropriate privileges
            // We log a warning but don't fail the processor startup
            tracing::warn!("Failed to set high thread priority: errno {}. This may require elevated privileges.", result);
            return Ok(()); // Don't fail - just run with normal priority
        }
    }

    tracing::info!("Applied high thread priority (SCHED_RR, priority 50)");
    Ok(())
}
