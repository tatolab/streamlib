// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use super::PythonMonotonicTimer;

#[derive(Clone, Copy)]
pub(super) struct MonotonicTimerFileDescriptors {
    timer_fd: i32,
    epoll_fd: i32,
}

/// Opens the timerfd and its epoll fd for a [`PythonMonotonicTimer`] whose first deadline is one
/// interval from now.
pub(super) fn open_python_monotonic_timer(interval_ns: i64) -> PyResult<PythonMonotonicTimer> {
    let file_descriptors =
        create_monotonic_timer_file_descriptors(interval_ns as u64).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "timerfd_create failed for interval_ns={interval_ns}"
            ))
        })?;
    Ok(PythonMonotonicTimer {
        timer_interval_ns: interval_ns,
        file_descriptors: Mutex::new(Some(file_descriptors)),
    })
}

/// Waits up to `timeout_ms` on the timer's epoll fd, as `PythonMonotonicTimer::wait` reports it.
pub(super) fn wait_on_python_monotonic_timer(
    timer: &PythonMonotonicTimer,
    python: Python<'_>,
    timeout_ms: u64,
) -> i64 {
    // Copied out rather than held: a close() racing this wait must
    // not block behind an epoll timeout, and a wait on
    // just-closed fds reports -1 through EBADF.
    let Some(file_descriptors) = *timer.file_descriptors.lock() else {
        return -1;
    };
    python.detach(move || wait_for_monotonic_timer_tick(file_descriptors, timeout_ms))
}

/// Closes the timer's fds, once.
pub(super) fn close_python_monotonic_timer(timer: &PythonMonotonicTimer) {
    if let Some(file_descriptors) = timer.file_descriptors.lock().take() {
        close_monotonic_timer_file_descriptors(file_descriptors);
    }
}

fn create_monotonic_timer_file_descriptors(
    interval_ns: u64,
) -> Option<MonotonicTimerFileDescriptors> {
    // SAFETY: plain fd-creating syscalls; failures surface as negative
    // returns handled below.
    //
    // Non-blocking, unlike the old subprocess timer: two threads woken by one
    // tick race the 8-byte read, and a blocking fd parks the loser until the
    // NEXT tick with no bound from `timeout_ms`. With `TFD_NONBLOCK` the loser
    // reads EAGAIN and reports a timeout instead.
    let timer_fd = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if timer_fd < 0 {
        return None;
    }

    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid stack slot.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } < 0 {
        // SAFETY: timer_fd was just opened by this function.
        unsafe { libc::close(timer_fd) };
        return None;
    }

    let timer_spec = absolute_timer_spec_one_interval_after(now, interval_ns);
    // SAFETY: timer_fd is a live timerfd; `timer_spec` is a valid stack slot.
    let arm_result = unsafe {
        libc::timerfd_settime(
            timer_fd,
            libc::TFD_TIMER_ABSTIME,
            &timer_spec,
            std::ptr::null_mut(),
        )
    };
    if arm_result < 0 {
        // SAFETY: timer_fd was just opened by this function.
        unsafe { libc::close(timer_fd) };
        return None;
    }

    // SAFETY: plain fd-creating syscall.
    let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll_fd < 0 {
        // SAFETY: timer_fd was just opened by this function.
        unsafe { libc::close(timer_fd) };
        return None;
    }
    let mut epoll_registration = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0,
    };
    // SAFETY: both fds are live; the event struct is a valid stack slot.
    if unsafe {
        libc::epoll_ctl(
            epoll_fd,
            libc::EPOLL_CTL_ADD,
            timer_fd,
            &mut epoll_registration,
        )
    } < 0
    {
        // SAFETY: both fds were just opened by this function.
        unsafe {
            libc::close(epoll_fd);
            libc::close(timer_fd);
        }
        return None;
    }

    Some(MonotonicTimerFileDescriptors { timer_fd, epoll_fd })
}

/// The `TFD_TIMER_ABSTIME` spec whose first deadline is `now + interval_ns`
/// and which repeats every `interval_ns` after it.
fn absolute_timer_spec_one_interval_after(
    now: libc::timespec,
    interval_ns: u64,
) -> libc::itimerspec {
    let interval_sec = (interval_ns / 1_000_000_000) as libc::time_t;
    let interval_nsec = (interval_ns % 1_000_000_000) as libc::c_long;
    let mut first_deadline_sec = now.tv_sec + interval_sec;
    let mut first_deadline_nsec = now.tv_nsec + interval_nsec;
    if first_deadline_nsec >= 1_000_000_000 {
        first_deadline_sec += 1;
        first_deadline_nsec -= 1_000_000_000;
    }
    libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: interval_sec,
            tv_nsec: interval_nsec,
        },
        it_value: libc::timespec {
            tv_sec: first_deadline_sec,
            tv_nsec: first_deadline_nsec,
        },
    }
}

fn wait_for_monotonic_timer_tick(
    file_descriptors: MonotonicTimerFileDescriptors,
    timeout_ms: u64,
) -> i64 {
    let mut ready_events = [libc::epoll_event { events: 0, u64: 0 }; 1];
    let bounded_timeout_ms = timeout_ms.min(i32::MAX as u64) as i32;
    // SAFETY: epoll_fd is live for the duration of the enclosing wait (a
    // racing close makes this return EBADF, reported as -1 below).
    let ready_count = unsafe {
        libc::epoll_wait(
            file_descriptors.epoll_fd,
            ready_events.as_mut_ptr(),
            1,
            bounded_timeout_ms,
        )
    };
    if ready_count < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            0
        } else {
            -1
        };
    }
    if ready_count == 0 {
        return 0;
    }
    let mut expiration_count: u64 = 0;
    // SAFETY: reading the timerfd's 8-byte expiration counter into a valid
    // stack slot.
    let read_result = unsafe {
        libc::read(
            file_descriptors.timer_fd,
            &mut expiration_count as *mut u64 as *mut libc::c_void,
            std::mem::size_of::<u64>(),
        )
    };
    if read_result < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
            0
        } else {
            -1
        };
    }
    expiration_count.min(i64::MAX as u64) as i64
}

fn close_monotonic_timer_file_descriptors(file_descriptors: MonotonicTimerFileDescriptors) {
    // SAFETY: the fds were created by `create_monotonic_timer_file_descriptors`
    // and taken out of the handle exactly once.
    unsafe {
        libc::close(file_descriptors.epoll_fd);
        libc::close(file_descriptors.timer_fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_linux_deadline_carries_nanoseconds_into_seconds() {
        let now = libc::timespec {
            tv_sec: 10,
            tv_nsec: 998_000_000,
        };

        let timer_spec = absolute_timer_spec_one_interval_after(now, 1_005_000_000);

        assert_eq!(
            (timer_spec.it_value.tv_sec, timer_spec.it_value.tv_nsec),
            (12, 3_000_000)
        );
        assert_eq!(
            (
                timer_spec.it_interval.tv_sec,
                timer_spec.it_interval.tv_nsec
            ),
            (1, 5_000_000)
        );
    }

    #[test]
    fn a_poll_before_the_first_deadline_reports_zero() {
        let file_descriptors = create_monotonic_timer_file_descriptors(3_600_000_000_000).unwrap();
        let expiration_count = wait_for_monotonic_timer_tick(file_descriptors, 0);
        close_monotonic_timer_file_descriptors(file_descriptors);
        assert_eq!(expiration_count, 0);
    }
}
