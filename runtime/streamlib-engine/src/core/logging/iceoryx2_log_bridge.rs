// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bridge iceoryx2-log records into the streamlib tracing pipeline.
//!
//! `iceoryx2` exposes its own logging trait via `iceoryx2-log`'s
//! [`Log`] interface plus a one-shot global `set_logger` that takes a
//! `&'static dyn Log`. Without a bridge, iceoryx2's internal log
//! records go to its default stderr logger and bypass the streamlib
//! JSONL pipeline entirely. Every process that takes an engine role
//! installs it: `Runner::new` in the app process, and
//! [`capture_this_helper_processes_engine_log_records`] in a helper.
//!
//! [`capture_this_helper_processes_engine_log_records`]:
//! crate::core::logging::capture_this_helper_processes_engine_log_records

use std::sync::atomic::{AtomicBool, Ordering};

use iceoryx2_log::Log;
use iceoryx2_log::LogLevel;
use tracing::level_filters::LevelFilter;

/// Zero-sized bridge implementing iceoryx2's [`Log`] trait by
/// forwarding records into the streamlib tracing pipeline.
pub struct IceoryxLogBridge;

impl Log for IceoryxLogBridge {
    fn log(
        &self,
        log_level: LogLevel,
        origin: core::fmt::Arguments,
        formatted_message: core::fmt::Arguments,
    ) {
        // `tracing::*!` macros take compile-time log levels; dispatch
        // through a match so iceoryx2's runtime LogLevel maps to the
        // matching tracing level. `Fatal` collapses to `Error` —
        // tracing has no separate fatal level and iceoryx2 emits
        // `Fatal` for genuinely-process-ending conditions that
        // iceoryx2 itself will abort on shortly after.
        match log_level {
            LogLevel::Trace => {
                tracing::trace!(target: "iceoryx2", origin = %origin, "{}", formatted_message)
            }
            LogLevel::Debug => {
                tracing::debug!(target: "iceoryx2", origin = %origin, "{}", formatted_message)
            }
            LogLevel::Info => {
                tracing::info!(target: "iceoryx2", origin = %origin, "{}", formatted_message)
            }
            LogLevel::Warn => {
                tracing::warn!(target: "iceoryx2", origin = %origin, "{}", formatted_message)
            }
            LogLevel::Error | LogLevel::Fatal => {
                tracing::error!(target: "iceoryx2", origin = %origin, "{}", formatted_message)
            }
        }
    }
}

/// Process-wide bridge value. Lives in `.rodata` (zero-sized); impls `Log`
/// against the workspace-pinned `iceoryx2-log-types::Log` vtable.
pub static HOST_BRIDGE: IceoryxLogBridge = IceoryxLogBridge;

/// Whether this process's iceoryx2 logger is [`HOST_BRIDGE`], so a second
/// install is read as the idempotent call it is rather than as iceoryx2
/// having found another logger first.
static THIS_PROCESSES_ICEORYX2_LOGGER_IS_THE_BRIDGE: AtomicBool = AtomicBool::new(false);

/// Install [`HOST_BRIDGE`] as iceoryx2's process-wide logger and set
/// iceoryx2's own level from the level this process's `tracing` subscriber
/// admits.
///
/// Idempotent — `iceoryx2_log::set_logger` is `Once`-guarded and answers
/// `false` on every later call. The level is set whatever the logger call
/// answers, and reading it from `tracing` rather than from `IOX2_LOG_LEVEL`
/// keeps one knob: `RUST_LOG` decides how much iceoryx2 says. The knob is the
/// most verbose level any target is configured for, not the `iceoryx2`
/// target's own, so a per-target filter has iceoryx2 format records the
/// subscriber then drops.
pub fn install_iceoryx2_log_bridge_at_the_engines_configured_level() {
    if iceoryx2_log::set_logger(&HOST_BRIDGE) {
        THIS_PROCESSES_ICEORYX2_LOGGER_IS_THE_BRIDGE.store(true, Ordering::Relaxed);
    } else if !THIS_PROCESSES_ICEORYX2_LOGGER_IS_THE_BRIDGE.load(Ordering::Relaxed) {
        tracing::warn!(
            "iceoryx2 already had a logger when the engine went to install its bridge, so \
             iceoryx2's records go to this process's standard error instead of into the log"
        );
    }
    if let Some(level) = iceoryx2_log_level_for(LevelFilter::current()) {
        iceoryx2_log::set_log_level(level);
    }
}

/// The iceoryx2 level that lets through exactly what `tracing_level` admits,
/// or `None` where nothing has said yet.
///
/// A process whose subscriber admits nothing has usually installed none:
/// `LevelFilter::current()` reads `OFF` until a dispatcher registers, which is
/// the state `STREAMLIB_DANGEROUSLY_DEFER_LOGGING_TO_HOST` leaves the engine
/// in. Pinning iceoryx2 to its quietest level on that reading would silence it
/// for a host that installs its own subscriber a moment later, so iceoryx2
/// keeps its own default instead.
fn iceoryx2_log_level_for(tracing_level: LevelFilter) -> Option<LogLevel> {
    match tracing_level {
        LevelFilter::TRACE => Some(LogLevel::Trace),
        LevelFilter::DEBUG => Some(LogLevel::Debug),
        LevelFilter::INFO => Some(LogLevel::Info),
        LevelFilter::WARN => Some(LogLevel::Warn),
        LevelFilter::ERROR => Some(LogLevel::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every level a subscriber can admit maps onto the iceoryx2 level that
    /// lets exactly that much through.
    #[test]
    fn each_tracing_level_maps_onto_the_iceoryx2_level_admitting_the_same_records() {
        assert_eq!(
            iceoryx2_log_level_for(LevelFilter::TRACE),
            Some(LogLevel::Trace)
        );
        assert_eq!(
            iceoryx2_log_level_for(LevelFilter::DEBUG),
            Some(LogLevel::Debug)
        );
        assert_eq!(
            iceoryx2_log_level_for(LevelFilter::INFO),
            Some(LogLevel::Info)
        );
        assert_eq!(
            iceoryx2_log_level_for(LevelFilter::WARN),
            Some(LogLevel::Warn)
        );
        assert_eq!(
            iceoryx2_log_level_for(LevelFilter::ERROR),
            Some(LogLevel::Error)
        );
    }

    /// Nothing admitted is also what a process with no subscriber yet reads,
    /// which is the host-owned-logging escape hatch: iceoryx2 keeps its own
    /// default there rather than being silenced for a host that installs its
    /// subscriber a moment later.
    #[test]
    fn a_process_whose_subscriber_admits_nothing_leaves_iceoryx2_level_alone() {
        assert_eq!(iceoryx2_log_level_for(LevelFilter::OFF), None);
    }
}
