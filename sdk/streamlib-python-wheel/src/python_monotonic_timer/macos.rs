// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::media_clock::MediaClock;

use super::PythonMonotonicTimer;
use super::monotonic_timer_absolute_deadline_grid::MonotonicTimerAbsoluteDeadlineGrid;
use crate::python_logging::monotonic_clock_now_ns;

/// The kqueue a macOS timer waits on, and where its absolute schedule stands.
pub(super) struct MonotonicTimerKqueueSchedule {
    kqueue_fd: OwnedFd,
    absolute_deadline_grid: MonotonicTimerAbsoluteDeadlineGrid,
}

/// Opens the kqueue for a [`PythonMonotonicTimer`] and arms its first deadline one interval from
/// now.
pub(super) fn open_python_monotonic_timer(interval_ns: i64) -> PyResult<PythonMonotonicTimer> {
    let kqueue_schedule =
        create_monotonic_timer_kqueue_schedule(interval_ns as u64).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "kqueue EVFILT_TIMER arm failed for interval_ns={interval_ns}"
            ))
        })?;
    Ok(PythonMonotonicTimer {
        timer_interval_ns: interval_ns,
        kqueue_schedule: Mutex::new(Some(kqueue_schedule)),
    })
}

/// Waits up to `timeout_ms` for the armed deadline, then counts what passed and arms the next,
/// as `PythonMonotonicTimer::wait` reports it.
pub(super) fn wait_on_python_monotonic_timer(
    timer: &PythonMonotonicTimer,
    python: Python<'_>,
    timeout_ms: u64,
) -> i64 {
    // Copied out rather than held: a close() racing this wait must not block
    // behind the kqueue timeout, and a wait on a just-closed kqueue reports -1
    // through EBADF. The schedule is relocked only once a deadline fired, to
    // count and re-arm.
    let Some(kqueue_fd) = timer
        .kqueue_schedule
        .lock()
        .as_ref()
        .map(|kqueue_schedule| kqueue_schedule.kqueue_fd.as_raw_fd())
    else {
        return -1;
    };
    match python.detach(move || wait_for_kqueue_timer_deadline(kqueue_fd, timeout_ms)) {
        KqueueTimerWaitOutcome::DeadlineReached => {}
        KqueueTimerWaitOutcome::TimedOut => return 0,
        KqueueTimerWaitOutcome::Failed => return -1,
    }
    match timer.kqueue_schedule.lock().as_mut() {
        Some(kqueue_schedule) => kqueue_schedule.take_expirations_and_arm_the_next_deadline(),
        None => -1,
    }
}

/// Closes the timer's kqueue, once.
pub(super) fn close_python_monotonic_timer(timer: &PythonMonotonicTimer) {
    drop(timer.kqueue_schedule.lock().take());
}

/// The one timer a monotonic-timer kqueue holds.
const MONOTONIC_TIMER_KEVENT_IDENT: libc::uintptr_t = 1;

enum KqueueTimerWaitOutcome {
    DeadlineReached,
    TimedOut,
    Failed,
}

impl MonotonicTimerKqueueSchedule {
    /// Deadlines passed since the last report, with the next unpassed one armed.
    fn take_expirations_and_arm_the_next_deadline(&mut self) -> i64 {
        let (expiration_count, next_deadline_ns) = self
            .absolute_deadline_grid
            .take_expirations_at(monotonic_clock_now_ns());
        if !arm_kqueue_timer_at_absolute_deadline(self.kqueue_fd.as_raw_fd(), next_deadline_ns) {
            return -1;
        }
        expiration_count.min(i64::MAX as u64) as i64
    }
}

fn create_monotonic_timer_kqueue_schedule(
    interval_ns: u64,
) -> Option<MonotonicTimerKqueueSchedule> {
    let kqueue_fd = crate::darwin_close_on_exec_kqueue::open_a_close_on_exec_kqueue().ok()?;
    let first_deadline_ns = monotonic_clock_now_ns().saturating_add(interval_ns);
    if !arm_kqueue_timer_at_absolute_deadline(kqueue_fd.as_raw_fd(), first_deadline_ns) {
        return None;
    }
    Some(MonotonicTimerKqueueSchedule {
        kqueue_fd,
        absolute_deadline_grid: MonotonicTimerAbsoluteDeadlineGrid {
            first_deadline_ns,
            interval_ns,
            deadlines_reported_count: 0,
        },
    })
}

/// Arms the kqueue's one-shot timer at `deadline_ns` on [`MediaClock`].
///
/// `NOTE_MACHTIME | NOTE_ABSOLUTE` takes the deadline in the
/// `mach_absolute_time` epoch; adding `NOTE_MACH_CONTINUOUS_TIME` would move it
/// to the continuous epoch, which runs ahead by every sleep since boot.
fn arm_kqueue_timer_at_absolute_deadline(kqueue_fd: i32, deadline_ns: u64) -> bool {
    arm_kqueue_timer_at_raw_mach_deadline(
        kqueue_fd,
        MediaClock::raw_timestamp_at_or_after_nanos(Duration::from_nanos(deadline_ns)),
    )
}

/// The one-shot timer change that fires at `raw_mach_deadline`, a
/// `mach_absolute_time` tick count.
fn kqueue_timer_arm_change_at_raw_mach_deadline(raw_mach_deadline: u64) -> libc::kevent {
    libc::kevent {
        ident: MONOTONIC_TIMER_KEVENT_IDENT,
        filter: libc::EVFILT_TIMER,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        // Without NOTE_CRITICAL the kernel coalesces the wake, which then lands
        // measurably late.
        fflags: libc::NOTE_MACHTIME | libc::NOTE_ABSOLUTE | libc::NOTE_CRITICAL,
        data: raw_mach_deadline.min(isize::MAX as u64) as isize,
        udata: std::ptr::null_mut(),
    }
}

fn arm_kqueue_timer_at_raw_mach_deadline(kqueue_fd: i32, raw_mach_deadline: u64) -> bool {
    let timer_arm_change = kqueue_timer_arm_change_at_raw_mach_deadline(raw_mach_deadline);
    // SAFETY: kqueue_fd is a live kqueue; the change list is one valid stack
    // slot and no events are requested back.
    let register_result = unsafe {
        libc::kevent(
            kqueue_fd,
            &timer_arm_change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    register_result >= 0
}

fn wait_for_kqueue_timer_deadline(kqueue_fd: i32, timeout_ms: u64) -> KqueueTimerWaitOutcome {
    let timeout = libc::timespec {
        tv_sec: (timeout_ms / 1_000).min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: ((timeout_ms % 1_000) * 1_000_000) as libc::c_long,
    };
    // SAFETY: an all-zero `kevent` is a valid out-slot.
    let mut ready_event: libc::kevent = unsafe { std::mem::zeroed() };
    // SAFETY: kqueue_fd is live for the duration of the enclosing wait (a
    // racing close makes this return EBADF, reported as Failed below); the
    // event and timeout are valid stack slots.
    let ready_count = unsafe {
        libc::kevent(
            kqueue_fd,
            std::ptr::null(),
            0,
            &mut ready_event,
            1,
            &timeout,
        )
    };
    if ready_count < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            KqueueTimerWaitOutcome::TimedOut
        } else {
            KqueueTimerWaitOutcome::Failed
        };
    }
    if ready_count == 0 {
        return KqueueTimerWaitOutcome::TimedOut;
    }
    if ready_event.flags & libc::EV_ERROR != 0 {
        return KqueueTimerWaitOutcome::Failed;
    }
    KqueueTimerWaitOutcome::DeadlineReached
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_before_the_first_deadline_times_out() {
        let kqueue_schedule = create_monotonic_timer_kqueue_schedule(3_600_000_000_000).unwrap();
        let outcome = wait_for_kqueue_timer_deadline(kqueue_schedule.kqueue_fd.as_raw_fd(), 0);
        drop(kqueue_schedule);
        assert!(matches!(outcome, KqueueTimerWaitOutcome::TimedOut));
    }

    #[test]
    fn the_kqueue_is_close_on_exec() {
        let kqueue_schedule = create_monotonic_timer_kqueue_schedule(3_600_000_000_000).unwrap();
        // SAFETY: querying descriptor flags on a live fd.
        let descriptor_flags =
            unsafe { libc::fcntl(kqueue_schedule.kqueue_fd.as_raw_fd(), libc::F_GETFD) };
        drop(kqueue_schedule);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
    }

    /// The deadline is read in the `mach_absolute_time` epoch, not the
    /// continuous one, which runs ahead of it by every sleep since boot.
    #[test]
    fn a_kqueue_deadline_is_armed_absolute_in_the_mach_absolute_time_epoch() {
        let timer_arm_change = kqueue_timer_arm_change_at_raw_mach_deadline(123_456_789);
        // `kevent` is packed on Darwin, so each field is copied out before it
        // is compared.
        let (armed_deadline, filter, flags, fflags) = (
            { timer_arm_change.data },
            { timer_arm_change.filter },
            { timer_arm_change.flags },
            { timer_arm_change.fflags },
        );

        assert_eq!(armed_deadline, 123_456_789);
        assert_eq!(filter, libc::EVFILT_TIMER);
        assert_eq!(flags, libc::EV_ADD | libc::EV_ONESHOT);
        assert_eq!(
            fflags,
            libc::NOTE_MACHTIME | libc::NOTE_ABSOLUTE | libc::NOTE_CRITICAL
        );
        assert_eq!(fflags & libc::NOTE_MACH_CONTINUOUS_TIME, 0);
    }
}
