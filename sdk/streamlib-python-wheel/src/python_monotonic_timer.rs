// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drift-free periodic timer for Python processors.
//!
//! The first deadline is now + interval and every repeat is absolute on the
//! engine's `MediaClock`, so latency in one tick never accumulates into the
//! next. Linux arms a `timerfd` with `TFD_TIMER_ABSTIME` and waits through an
//! epoll fd; macOS arms a one-shot kqueue `EVFILT_TIMER` at each next deadline
//! in `mach_absolute_time` ticks. Either wait honors a timeout, which is what
//! bounds teardown latency for a loop polling shutdown between ticks.

use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
#[cfg(target_os = "macos")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "macos")]
use std::time::Duration;
#[cfg(target_os = "macos")]
use streamlib::sdk::media_clock::MediaClock;

#[cfg(target_os = "macos")]
use crate::python_logging::monotonic_clock_now_ns;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct MonotonicTimerFileDescriptors {
    timer_fd: i32,
    epoll_fd: i32,
}

/// The kqueue a macOS timer waits on, and where its absolute schedule stands.
#[cfg(target_os = "macos")]
struct MonotonicTimerKqueueSchedule {
    kqueue_fd: OwnedFd,
    first_deadline_ns: u64,
    interval_ns: u64,
    deadlines_reported_count: u64,
}

/// Periodic monotonic timer, used as `with MonotonicTimer(interval_ns) as t:`.
#[pyclass(name = "MonotonicTimer", module = "streamlib", frozen)]
pub(crate) struct PythonMonotonicTimer {
    timer_interval_ns: i64,
    #[cfg(target_os = "linux")]
    file_descriptors: Mutex<Option<MonotonicTimerFileDescriptors>>,
    #[cfg(target_os = "macos")]
    kqueue_schedule: Mutex<Option<MonotonicTimerKqueueSchedule>>,
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
        #[cfg(target_os = "macos")]
        {
            let kqueue_schedule = create_monotonic_timer_kqueue_schedule(interval_ns as u64)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "kqueue EVFILT_TIMER arm failed for interval_ns={interval_ns}"
                    ))
                })?;
            Ok(Self {
                timer_interval_ns: interval_ns,
                kqueue_schedule: Mutex::new(Some(kqueue_schedule)),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(PyRuntimeError::new_err(
                "MonotonicTimer has no timer backend on this platform",
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
        #[cfg(target_os = "macos")]
        {
            // Copied out for the same reason as the Linux arm; the schedule is
            // relocked only once a deadline fired, to count and re-arm.
            let Some(kqueue_fd) = self
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
            match self.kqueue_schedule.lock().as_mut() {
                Some(kqueue_schedule) => {
                    kqueue_schedule.take_expirations_and_arm_the_next_deadline()
                }
                None => -1,
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
        #[cfg(target_os = "macos")]
        drop(self.kqueue_schedule.lock().take());
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

/// The one timer a monotonic-timer kqueue holds.
#[cfg(target_os = "macos")]
const MONOTONIC_TIMER_KEVENT_IDENT: libc::uintptr_t = 1;

#[cfg(target_os = "macos")]
enum KqueueTimerWaitOutcome {
    DeadlineReached,
    TimedOut,
    Failed,
}

#[cfg(target_os = "macos")]
impl MonotonicTimerKqueueSchedule {
    /// Deadlines passed since the last report, with the next unpassed one armed.
    fn take_expirations_and_arm_the_next_deadline(&mut self) -> i64 {
        let now_ns = monotonic_clock_now_ns();
        let deadlines_passed_count = match now_ns.checked_sub(self.first_deadline_ns) {
            Some(since_first_deadline_ns) => since_first_deadline_ns / self.interval_ns + 1,
            None => 0,
        };
        let expiration_count = deadlines_passed_count.saturating_sub(self.deadlines_reported_count);
        self.deadlines_reported_count = self.deadlines_reported_count.max(deadlines_passed_count);
        let next_deadline_ns = self.first_deadline_ns.saturating_add(
            self.deadlines_reported_count
                .saturating_mul(self.interval_ns),
        );
        if !arm_kqueue_timer_at_absolute_deadline(self.kqueue_fd.as_raw_fd(), next_deadline_ns) {
            return -1;
        }
        expiration_count.min(i64::MAX as u64) as i64
    }
}

#[cfg(target_os = "macos")]
fn create_monotonic_timer_kqueue_schedule(
    interval_ns: u64,
) -> Option<MonotonicTimerKqueueSchedule> {
    // SAFETY: plain fd-creating syscall; failure surfaces as a negative return.
    let raw_kqueue_fd = unsafe { libc::kqueue() };
    if raw_kqueue_fd < 0 {
        return None;
    }
    // SAFETY: raw_kqueue_fd was just opened by this function and nothing else
    // owns it.
    let kqueue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };
    // Darwin has no `kqueue1`, so close-on-exec is set before the fd is used.
    // SAFETY: fcntl on a live fd this function owns.
    if unsafe { libc::fcntl(kqueue_fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return None;
    }
    let first_deadline_ns = monotonic_clock_now_ns().saturating_add(interval_ns);
    if !arm_kqueue_timer_at_absolute_deadline(kqueue_fd.as_raw_fd(), first_deadline_ns) {
        return None;
    }
    Some(MonotonicTimerKqueueSchedule {
        kqueue_fd,
        first_deadline_ns,
        interval_ns,
        deadlines_reported_count: 0,
    })
}

/// Arms the kqueue's one-shot timer at `deadline_ns` on [`MediaClock`].
///
/// `NOTE_MACHTIME | NOTE_ABSOLUTE` takes the deadline in the
/// `mach_absolute_time` epoch; adding `NOTE_MACH_CONTINUOUS_TIME` would move it
/// to the continuous epoch, which runs ahead by every sleep since boot.
#[cfg(target_os = "macos")]
fn arm_kqueue_timer_at_absolute_deadline(kqueue_fd: i32, deadline_ns: u64) -> bool {
    arm_kqueue_timer_at_raw_mach_deadline(
        kqueue_fd,
        MediaClock::raw_timestamp_at_or_after_nanos(Duration::from_nanos(deadline_ns)),
    )
}

#[cfg(target_os = "macos")]
fn arm_kqueue_timer_at_raw_mach_deadline(kqueue_fd: i32, raw_mach_deadline: u64) -> bool {
    let timer_arm_change = libc::kevent {
        ident: MONOTONIC_TIMER_KEVENT_IDENT,
        filter: libc::EVFILT_TIMER,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        // Without NOTE_CRITICAL the kernel coalesces the wake, which then lands
        // measurably late.
        fflags: libc::NOTE_MACHTIME | libc::NOTE_ABSOLUTE | libc::NOTE_CRITICAL,
        data: raw_mach_deadline.min(isize::MAX as u64) as isize,
        udata: std::ptr::null_mut(),
    };
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

#[cfg(target_os = "macos")]
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_kqueue_tick_fires_within_the_wait_window() {
        let mut kqueue_schedule = create_monotonic_timer_kqueue_schedule(2_000_000).unwrap();
        assert!(matches!(
            wait_for_kqueue_timer_deadline(kqueue_schedule.kqueue_fd.as_raw_fd(), 1_000),
            KqueueTimerWaitOutcome::DeadlineReached
        ));
        let expiration_count = kqueue_schedule.take_expirations_and_arm_the_next_deadline();
        drop(kqueue_schedule);
        assert!(
            expiration_count >= 1,
            "a 2ms timer's deadline fired but counted no expiration: {expiration_count}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_kqueue_wait_shorter_than_the_interval_times_out() {
        let kqueue_schedule = create_monotonic_timer_kqueue_schedule(10_000_000_000).unwrap();
        let outcome = wait_for_kqueue_timer_deadline(kqueue_schedule.kqueue_fd.as_raw_fd(), 10);
        drop(kqueue_schedule);
        assert!(matches!(outcome, KqueueTimerWaitOutcome::TimedOut));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_kqueue_is_close_on_exec() {
        let kqueue_schedule = create_monotonic_timer_kqueue_schedule(10_000_000_000).unwrap();
        // SAFETY: querying descriptor flags on a live fd.
        let descriptor_flags =
            unsafe { libc::fcntl(kqueue_schedule.kqueue_fd.as_raw_fd(), libc::F_GETFD) };
        drop(kqueue_schedule);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
    }

    /// The deadline is read in the `mach_absolute_time` epoch, not the
    /// continuous one, which runs ahead of it by every sleep since boot.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_kqueue_deadline_is_armed_in_the_mach_absolute_time_epoch() {
        const DEADLINE_AHEAD_NS: u64 = 300_000_000;
        let deadline_ahead_ticks =
            MediaClock::raw_timestamp_at_or_after_nanos(Duration::from_nanos(DEADLINE_AHEAD_NS));
        // SAFETY: plain fd-creating syscall.
        let raw_kqueue_fd = unsafe { libc::kqueue() };
        assert!(raw_kqueue_fd >= 0);
        // SAFETY: raw_kqueue_fd was just opened here and nothing else owns it.
        let kqueue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };

        let armed_at_ns = monotonic_clock_now_ns();
        assert!(arm_kqueue_timer_at_raw_mach_deadline(
            kqueue_fd.as_raw_fd(),
            MediaClock::raw_timestamp() + deadline_ahead_ticks,
        ));
        let outcome = wait_for_kqueue_timer_deadline(kqueue_fd.as_raw_fd(), 2_000);
        let fired_after_ns = monotonic_clock_now_ns() - armed_at_ns;
        assert!(matches!(outcome, KqueueTimerWaitOutcome::DeadlineReached));
        assert!(
            fired_after_ns >= DEADLINE_AHEAD_NS,
            "a deadline {DEADLINE_AHEAD_NS}ns ahead fired after {fired_after_ns}ns"
        );

        // The control: where this host has slept for longer than the
        // deadline's lead, the same deadline read on the continuous epoch is
        // already past, so the check above tells the two epochs apart here.
        // SAFETY: `mach_continuous_time` takes no arguments and cannot fail.
        let continuous_lead_ticks = unsafe { mach_continuous_time() } - MediaClock::raw_timestamp();
        if continuous_lead_ticks > 2 * deadline_ahead_ticks {
            let control_armed_at_ns = monotonic_clock_now_ns();
            let continuous_epoch_arm_change = libc::kevent {
                ident: MONOTONIC_TIMER_KEVENT_IDENT,
                filter: libc::EVFILT_TIMER,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_MACHTIME
                    | libc::NOTE_ABSOLUTE
                    | libc::NOTE_CRITICAL
                    | libc::NOTE_MACH_CONTINUOUS_TIME,
                data: (MediaClock::raw_timestamp() + deadline_ahead_ticks) as isize,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: a live kqueue, one valid change slot, no events back.
            let register_result = unsafe {
                libc::kevent(
                    kqueue_fd.as_raw_fd(),
                    &continuous_epoch_arm_change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            assert!(register_result >= 0);
            let control_outcome = wait_for_kqueue_timer_deadline(kqueue_fd.as_raw_fd(), 2_000);
            let control_fired_after_ns = monotonic_clock_now_ns() - control_armed_at_ns;
            assert!(matches!(
                control_outcome,
                KqueueTimerWaitOutcome::DeadlineReached
            ));
            assert!(
                control_fired_after_ns < DEADLINE_AHEAD_NS / 2,
                "a continuous-epoch control fired after {control_fired_after_ns}ns, so this \
                 check cannot tell the epochs apart"
            );
        }
    }

    /// Late wakes never push later deadlines out. Each wake is measured
    /// against its own absolute deadline, so a schedule re-armed relative to
    /// the wake shows up as lateness that grows period over period.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_kqueue_timer_does_not_drift_across_many_periods() {
        const INTERVAL_NS: u64 = 5_000_000;
        const PERIOD_COUNT: u64 = 400;
        const PER_TICK_WORK: Duration = Duration::from_millis(1);
        const FINAL_WAKES_MEASURED: usize = 50;
        const MEDIAN_FINAL_LATENESS_BOUND_NS: u64 = 1_000_000;
        let mut kqueue_schedule = create_monotonic_timer_kqueue_schedule(INTERVAL_NS).unwrap();
        let mut expirations_seen: u64 = 0;
        let mut wake_lateness_ns = Vec::new();
        while expirations_seen < PERIOD_COUNT {
            match wait_for_kqueue_timer_deadline(kqueue_schedule.kqueue_fd.as_raw_fd(), 1_000) {
                KqueueTimerWaitOutcome::DeadlineReached => {}
                KqueueTimerWaitOutcome::TimedOut => panic!("no deadline inside a 1s wait"),
                KqueueTimerWaitOutcome::Failed => panic!("the kqueue wait failed"),
            }
            let woke_at_ns = monotonic_clock_now_ns();
            let expiration_count = kqueue_schedule.take_expirations_and_arm_the_next_deadline();
            assert!(
                expiration_count >= 1,
                "a wake reported {expiration_count} expirations — it fired before its deadline"
            );
            expirations_seen += expiration_count as u64;
            let latest_deadline_ns =
                kqueue_schedule.first_deadline_ns + (expirations_seen - 1) * INTERVAL_NS;
            wake_lateness_ns.push(woke_at_ns - latest_deadline_ns);
            std::thread::sleep(PER_TICK_WORK);
        }
        drop(kqueue_schedule);

        let mut final_wake_lateness_ns = wake_lateness_ns
            [wake_lateness_ns.len().saturating_sub(FINAL_WAKES_MEASURED)..]
            .to_vec();
        final_wake_lateness_ns.sort_unstable();
        let median_final_lateness_ns = final_wake_lateness_ns[final_wake_lateness_ns.len() / 2];
        assert!(
            median_final_lateness_ns < MEDIAN_FINAL_LATENESS_BOUND_NS,
            "after {PERIOD_COUNT} periods the median wake is {median_final_lateness_ns}ns past its \
             absolute deadline — the schedule drifted"
        );
    }
}
