// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Realtime scheduling for the calling thread, asked for the way the engine asks:
//! rtkit over the system bus, else `SCHED_FIFO` directly.

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const RTKIT_SERVICE: &str = "org.freedesktop.RealtimeKit1";
const RTKIT_PATH: &str = "/org/freedesktop/RealtimeKit1";
const RTKIT_INTERFACE: &str = "org.freedesktop.RealtimeKit1";
const DBUS_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

/// `SCHED_FIFO` priority the engine's own direct fallback asks for.
const DIRECT_SCHED_FIFO_PRIORITY: i32 = 80;

/// rtkit's own default when it does not say.
const RTKIT_DEFAULT_MAX_REALTIME_PRIORITY: u32 = 20;

/// How the calling thread ended up scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RealtimeThreadPriorityOutcome {
    GrantedByRtkit { priority: u32 },
    GrantedDirectly { priority: i32 },
    Refused { rtkit_refusal: String },
}

impl RealtimeThreadPriorityOutcome {
    pub(crate) fn description(&self) -> String {
        match self {
            Self::GrantedByRtkit { priority } => format!("rtkit SCHED_RR {priority}"),
            Self::GrantedDirectly { priority } => format!("direct SCHED_FIFO {priority}"),
            Self::Refused { rtkit_refusal } => {
                format!("not realtime (rtkit: {rtkit_refusal}; direct SCHED_FIFO refused)")
            }
        }
    }
}

/// Make the calling thread realtime, or say why it stayed where it was.
pub(crate) fn make_the_calling_thread_realtime() -> RealtimeThreadPriorityOutcome {
    match make_the_calling_thread_realtime_through_rtkit() {
        Ok(priority) => RealtimeThreadPriorityOutcome::GrantedByRtkit { priority },
        Err(rtkit_refusal) => {
            if make_the_calling_thread_sched_fifo_directly() {
                RealtimeThreadPriorityOutcome::GrantedDirectly {
                    priority: DIRECT_SCHED_FIFO_PRIORITY,
                }
            } else {
                RealtimeThreadPriorityOutcome::Refused { rtkit_refusal }
            }
        }
    }
}

fn make_the_calling_thread_realtime_through_rtkit() -> Result<u32, String> {
    let connection = Connection::system().map_err(|error| format!("no system bus: {error}"))?;
    let rtkit = Proxy::new(&connection, RTKIT_SERVICE, RTKIT_PATH, RTKIT_INTERFACE)
        .map_err(|error| error.to_string())?;
    let properties = Proxy::new(
        &connection,
        RTKIT_SERVICE,
        RTKIT_PATH,
        DBUS_PROPERTIES_INTERFACE,
    )
    .map_err(|error| error.to_string())?;
    let rtkit_property = |name: &str| -> zbus::Result<OwnedValue> {
        properties.call("Get", &(RTKIT_INTERFACE, name))
    };

    // rtkit refuses a thread whose process has no RLIMIT_RTTIME at or under its own cap.
    let rttime_cap_us = rtkit_property("RTTimeUSecMax")
        .ok()
        .and_then(|value| i64::try_from(&value).ok())
        .ok_or("rtkit did not say RTTimeUSecMax")?
        .max(0) as u64;
    let rttime_limit = libc::rlimit {
        rlim_cur: rttime_cap_us,
        rlim_max: rttime_cap_us,
    };
    // SAFETY: the limit is a valid struct; the call only reads it.
    unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &rttime_limit) };

    let priority = rtkit_property("MaxRealtimePriority")
        .ok()
        .and_then(|value| i32::try_from(&value).ok())
        .and_then(|priority| u32::try_from(priority).ok())
        .filter(|priority| *priority > 0)
        .unwrap_or(RTKIT_DEFAULT_MAX_REALTIME_PRIORITY);
    // SAFETY: `gettid` takes no arguments and cannot fail.
    let thread_id = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
    rtkit
        .call::<_, _, ()>("MakeThreadRealtime", &(thread_id, priority))
        .map_err(|error| error.to_string())?;
    Ok(priority)
}

fn make_the_calling_thread_sched_fifo_directly() -> bool {
    // SAFETY: a zeroed `sched_param` is valid, and the call only reads it.
    unsafe {
        let mut parameters: libc::sched_param = std::mem::zeroed();
        parameters.sched_priority = DIRECT_SCHED_FIFO_PRIORITY;
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &parameters) == 0
    }
}
