// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drift-free periodic timer for Python processors.
//!
//! `timerfd_create(CLOCK_MONOTONIC)` + `TFD_TIMER_ABSTIME`: the first
//! deadline is now + interval and every repeat is absolute, so latency in
//! one tick never accumulates into the next. An epoll fd alongside lets
//! `wait` honor a timeout, which is what bounds teardown latency for a loop
//! polling shutdown between ticks. Linux-only, like the platform floor.

use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct MonotonicTimerFileDescriptors {
    timer_fd: i32,
    epoll_fd: i32,
}

/// Periodic monotonic timer, used as `with MonotonicTimer(interval_ns) as t:`.
#[pyclass(name = "MonotonicTimer", module = "streamlib", frozen)]
pub(crate) struct PythonMonotonicTimer {
    timer_interval_ns: i64,
    #[cfg(target_os = "linux")]
    file_descriptors: Mutex<Option<MonotonicTimerFileDescriptors>>,
    #[cfg(not(target_os = "linux"))]
    file_descriptors: Mutex<Option<()>>,
}

#[pymethods]
impl PythonMonotonicTimer {
    #[new]
    fn new(interval_ns: i64) -> PyResult<Self> {
        if interval_ns <= 0 {
            return Err(PyValueError::new_err(format!(
                "interval_ns must be > 0, got {interval_ns}"
            )));
        }
        #[cfg(target_os = "linux")]
        {
            let file_descriptors = create_monotonic_timer_file_descriptors(interval_ns as u64)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "timerfd_create failed for interval_ns={interval_ns}"
                    ))
                })?;
            Ok(Self {
                timer_interval_ns: interval_ns,
                file_descriptors: Mutex::new(Some(file_descriptors)),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(PyRuntimeError::new_err(
                "MonotonicTimer is Linux-only — timerfd does not exist on this platform",
            ))
        }
    }

    #[getter]
    fn interval_ns(&self) -> i64 {
        self.timer_interval_ns
    }

    /// Wait up to `timeout_ms` for the next tick.
    ///
    /// Returns the positive expiration count when a tick fired, `0` on
    /// timeout (poll shutdown and call again), `-1` after `close()` or on
    /// error.
    #[pyo3(signature = (timeout_ms = 100))]
    fn wait(&self, python: Python<'_>, timeout_ms: u64) -> i64 {
        #[cfg(target_os = "linux")]
        {
            // Copied out rather than held: a close() racing this wait must
            // not block behind an epoll timeout, and a wait on
            // just-closed fds reports -1 through EBADF.
            let Some(file_descriptors) = *self.file_descriptors.lock() else {
                return -1;
            };
            python.detach(move || wait_for_monotonic_timer_tick(file_descriptors, timeout_ms))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, timeout_ms);
            -1
        }
    }

    /// Close the timer. Idempotent; a subsequent `wait` returns `-1`.
    fn close(&self) {
        #[cfg(target_os = "linux")]
        if let Some(file_descriptors) = self.file_descriptors.lock().take() {
            close_monotonic_timer_file_descriptors(file_descriptors);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self.file_descriptors.lock().take();
        }
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    #[pyo3(signature = (*_exception_details))]
    fn __exit__(&self, _exception_details: &Bound<'_, PyAny>) -> bool {
        self.close();
        false
    }
}

impl Drop for PythonMonotonicTimer {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(target_os = "linux")]
fn create_monotonic_timer_file_descriptors(
    interval_ns: u64,
) -> Option<MonotonicTimerFileDescriptors> {
    let interval_sec = (interval_ns / 1_000_000_000) as libc::time_t;
    let interval_nsec = (interval_ns % 1_000_000_000) as libc::c_long;

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

    let mut first_deadline_sec = now.tv_sec + interval_sec;
    let mut first_deadline_nsec = now.tv_nsec + interval_nsec;
    if first_deadline_nsec >= 1_000_000_000 {
        first_deadline_sec += 1;
        first_deadline_nsec -= 1_000_000_000;
    }

    let timer_spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: interval_sec,
            tv_nsec: interval_nsec,
        },
        it_value: libc::timespec {
            tv_sec: first_deadline_sec,
            tv_nsec: first_deadline_nsec,
        },
    };
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn close_monotonic_timer_file_descriptors(file_descriptors: MonotonicTimerFileDescriptors) {
    // SAFETY: the fds were created by `create_monotonic_timer_file_descriptors`
    // and taken out of the handle exactly once.
    unsafe {
        libc::close(file_descriptors.epoll_fd);
        libc::close(file_descriptors.timer_fd);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_tick_fires_within_the_wait_window() {
        let file_descriptors = create_monotonic_timer_file_descriptors(2_000_000).unwrap();
        let expiration_count = wait_for_monotonic_timer_tick(file_descriptors, 1_000);
        close_monotonic_timer_file_descriptors(file_descriptors);
        assert!(
            expiration_count >= 1,
            "a 2ms timer produced no tick inside a 1s wait: {expiration_count}"
        );
    }

    #[test]
    fn a_wait_shorter_than_the_interval_times_out_with_zero() {
        let file_descriptors = create_monotonic_timer_file_descriptors(10_000_000_000).unwrap();
        let expiration_count = wait_for_monotonic_timer_tick(file_descriptors, 10);
        close_monotonic_timer_file_descriptors(file_descriptors);
        assert_eq!(expiration_count, 0);
    }

    #[test]
    fn a_closed_timer_reports_minus_one() {
        Python::initialize();
        Python::attach(|python| {
            let timer = PythonMonotonicTimer::new(2_000_000).unwrap();
            timer.close();
            assert_eq!(timer.wait(python, 10), -1);
        });
    }

    #[test]
    fn a_non_positive_interval_is_refused() {
        assert!(PythonMonotonicTimer::new(0).is_err());
        assert!(PythonMonotonicTimer::new(-5).is_err());
    }
}
