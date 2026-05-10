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
use zbus::zvariant::OwnedValue;

const SERVICE: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const INTERFACE: &str = "org.freedesktop.RealtimeKit1";

/// Lazily-initialised connection + proxy pair: one for rtkit's own
/// methods, one for the standard `org.freedesktop.DBus.Properties`
/// interface so we can call `Get` directly.
///
/// rtkit-daemon's Properties implementation is partial — it only
/// implements `Get`, not `GetAll`. zbus's `Proxy::get_property`
/// internally calls `GetAll`, which rtkit rejects with
/// `org.freedesktop.DBus.Error.UnknownMethod`. We bypass that by
/// holding a Properties-interface proxy and invoking `Get` ourselves.
struct RtkitProxies {
    rtkit: Proxy<'static>,
    properties: Proxy<'static>,
}

fn proxies() -> Option<&'static RtkitProxies> {
    static CACHED: OnceLock<Option<RtkitProxies>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let conn = match Connection::system() {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("rtkit unavailable: no system bus connection ({e})");
                    return None;
                }
            };
            let rtkit = match Proxy::new(&conn, SERVICE, PATH, INTERFACE) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!("rtkit unavailable: rtkit proxy creation failed ({e})");
                    return None;
                }
            };
            let properties =
                match Proxy::new(&conn, SERVICE, PATH, "org.freedesktop.DBus.Properties") {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::debug!(
                            "rtkit unavailable: Properties proxy creation failed ({e})"
                        );
                        return None;
                    }
                };
            Some(RtkitProxies { rtkit, properties })
        })
        .as_ref()
}

/// Direct `Properties.Get` call — bypasses zbus's `get_property`
/// caching path which uses `GetAll` (unimplemented in rtkit-daemon).
fn get_rtkit_property(props: &Proxy<'static>, name: &str) -> zbus::Result<OwnedValue> {
    props.call::<_, _, OwnedValue>("Get", &(INTERFACE, name))
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
fn ensure_rttime_limit_set(props: &Proxy<'static>) -> zbus::Result<()> {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Acquire) {
        return Ok(());
    }

    let value = get_rtkit_property(props, "RTTimeUSecMax")?;
    let max_usec: i64 = i64::try_from(&value).map_err(|e| {
        zbus::Error::Variant(zbus::zvariant::Error::Message(format!(
            "RTTimeUSecMax not i64: {e}"
        )))
    })?;
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

/// Read rtkit's `MaxRealtimePriority` policy ceiling. Subsequent calls
/// can request anywhere in `[1, max]`; rtkit refuses anything above.
/// Defaults to 20 (rtkit-daemon's compiled-in default) on read failure.
fn max_realtime_priority(props: &Proxy<'static>) -> u32 {
    match get_rtkit_property(props, "MaxRealtimePriority")
        .ok()
        .and_then(|v| i32::try_from(&v).ok())
    {
        Some(v) if v > 0 => v as u32,
        _ => 20,
    }
}

/// Read rtkit's `MinNiceLevel` policy floor (most-negative nice rtkit
/// will grant). Defaults to -15 on read failure.
fn min_nice_level(props: &Proxy<'static>) -> i32 {
    get_rtkit_property(props, "MinNiceLevel")
        .ok()
        .and_then(|v| i32::try_from(&v).ok())
        .unwrap_or(-15)
}

/// Ask rtkit to schedule the current thread as `SCHED_RR` at the
/// highest priority rtkit's policy allows (typically 20 on stock
/// rtkit-daemon installs). Returns `Err` if rtkit isn't reachable or
/// rejects the request.
///
/// rtkit's policy cap is what matters; the absolute priority value
/// within `SCHED_RR` only orders RT threads against each other, and
/// streamlib has a single RealTime tier — so taking rtkit's max
/// gives the strongest preemption rtkit will broker without the call
/// being refused.
pub fn make_current_thread_realtime() -> Result<(), String> {
    let RtkitProxies { rtkit, properties } =
        proxies().ok_or_else(|| "rtkit not available on system bus".to_string())?;

    if let Err(e) = ensure_rttime_limit_set(properties) {
        return Err(format!("rtkit: failed to read RTTimeUSecMax: {e}"));
    }

    let priority = max_realtime_priority(properties);
    let tid = current_tid();
    rtkit
        .call::<_, _, ()>("MakeThreadRealtime", &(tid, priority))
        .map_err(|e| format!("rtkit::MakeThreadRealtime(tid={tid}, prio={priority}): {e}"))
}

/// Ask rtkit to schedule the current thread as `SCHED_OTHER` at the
/// lowest (most-negative) nice level rtkit's policy allows (typically
/// -15 on stock rtkit-daemon installs). Returns `Err` if rtkit isn't
/// reachable or rejects the request.
pub fn make_current_thread_high_priority() -> Result<(), String> {
    let RtkitProxies { rtkit, properties } =
        proxies().ok_or_else(|| "rtkit not available on system bus".to_string())?;

    let niceness = min_nice_level(properties);
    let tid = current_tid();
    rtkit
        .call::<_, _, ()>("MakeThreadHighPriority", &(tid, niceness))
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
        let _ = make_current_thread_realtime();
    }

    #[test]
    fn make_thread_high_priority_is_graceful_when_rtkit_unreachable_or_grants() {
        let _ = make_current_thread_high_priority();
    }
}
