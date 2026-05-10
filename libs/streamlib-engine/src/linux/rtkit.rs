// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RealtimeKit D-Bus client for unprivileged thread priority elevation.
//!
//! `pthread_setschedparam(SCHED_FIFO|SCHED_RR, ...)` on Linux requires
//! `CAP_SYS_NICE` — every desktop process without elevated privileges
//! gets `EPERM`. The `rtkit-daemon` (systemd-shipped, present on every
//! modern desktop because PipeWire / PulseAudio / JACK depend on it)
//! runs as root, accepts D-Bus requests on
//! `org.freedesktop.RealtimeKit1`, performs basic policy checks, and
//! issues the privileged `sched_setscheduler` / `setpriority` call on
//! the requesting thread's behalf. The application stays unprivileged.
//!
//! This module wraps the two methods we care about:
//! - `MakeThreadRealtime(tid, priority)` — SCHED_RR with `priority` in [1, 99]
//! - `MakeThreadHighPriority(tid, niceness)` — nice level adjustment in [-20, 19]
//!
//! Both fall through to `Err` when rtkit isn't running, isn't reachable,
//! or refuses the request. Callers should fall back to the direct
//! `pthread_setschedparam` path (which will likely fail with `EPERM`,
//! at which point logging the warning and continuing on `SCHED_OTHER`
//! is the documented behavior).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use zbus::blocking::{Connection, Proxy};

const SERVICE: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const INTERFACE: &str = "org.freedesktop.RealtimeKit1";

/// Lazily-initialised connection + proxy. `None` when rtkit isn't
/// reachable on the system bus; callers should fall back.
fn proxy() -> Option<&'static Proxy<'static>> {
    static CACHED: OnceLock<Option<Proxy<'static>>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let conn = match Connection::system() {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("rtkit unavailable: no system bus connection ({e})");
                    return None;
                }
            };
            match Proxy::new(&conn, SERVICE, PATH, INTERFACE) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::debug!("rtkit unavailable: proxy creation failed ({e})");
                    None
                }
            }
        })
        .as_ref()
}

/// `gettid()` — Linux-specific kernel thread id. `pthread_self()` is a
/// libc handle, not what rtkit / `sched_setscheduler` expect.
fn current_tid() -> u64 {
    // SAFETY: `SYS_gettid` is parameter-less and infallible.
    unsafe { libc::syscall(libc::SYS_gettid) as u64 }
}

/// Rtkit requires the caller to have set `RLIMIT_RTTIME` to a finite
/// value before granting realtime priority — a safety net against
/// runaway RT loops monopolising a CPU. Read rtkit's own
/// `RTTimeUSecMax` property and set the process rlimit to it.
///
/// Done once per process; the rlimit is process-wide. Subsequent calls
/// are no-ops.
fn ensure_rttime_limit_set(p: &Proxy<'static>) -> zbus::Result<()> {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Acquire) {
        return Ok(());
    }

    let max_usec: i64 = p.get_property("RTTimeUSecMax")?;
    let max_usec: u64 = max_usec.max(0) as u64;

    let rl = libc::rlimit {
        rlim_cur: max_usec,
        rlim_max: max_usec,
    };
    // SAFETY: `RLIMIT_RTTIME` accepts a `rlimit` by pointer; struct is
    // fully initialised above.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &rl) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            "rtkit: setrlimit(RLIMIT_RTTIME, {max_usec}) failed: {err} \
             — rtkit may reject the realtime request"
        );
    }

    DONE.store(true, Ordering::Release);
    Ok(())
}

/// Ask rtkit to schedule the current thread as `SCHED_RR` with the
/// given priority. Returns `Err` if rtkit isn't reachable or rejects
/// the request.
pub fn make_current_thread_realtime(priority: u32) -> Result<(), String> {
    let p = proxy().ok_or_else(|| "rtkit not available on system bus".to_string())?;

    if let Err(e) = ensure_rttime_limit_set(p) {
        return Err(format!("rtkit: failed to read RTTimeUSecMax: {e}"));
    }

    let tid = current_tid();
    p.call::<_, _, ()>("MakeThreadRealtime", &(tid, priority))
        .map_err(|e| format!("rtkit::MakeThreadRealtime(tid={tid}, prio={priority}): {e}"))
}

/// Ask rtkit to schedule the current thread as `SCHED_OTHER` with the
/// given nice level (negative = higher priority). Returns `Err` if
/// rtkit isn't reachable or rejects the request.
pub fn make_current_thread_high_priority(niceness: i32) -> Result<(), String> {
    let p = proxy().ok_or_else(|| "rtkit not available on system bus".to_string())?;

    let tid = current_tid();
    p.call::<_, _, ()>("MakeThreadHighPriority", &(tid, niceness))
        .map_err(|e| format!("rtkit::MakeThreadHighPriority(tid={tid}, nice={niceness}): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rtkit may or may not be running in the test environment; both
    /// paths must be exercise-safe. The function returns `Err`
    /// cleanly without panicking when rtkit isn't reachable.
    #[test]
    fn make_thread_realtime_is_graceful_when_rtkit_unreachable_or_grants() {
        // We don't assert success — CI may or may not have rtkit. We
        // just confirm the call doesn't panic and returns a Result.
        let _ = make_current_thread_realtime(20);
    }

    #[test]
    fn make_thread_high_priority_is_graceful_when_rtkit_unreachable_or_grants() {
        let _ = make_current_thread_high_priority(-5);
    }
}
