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
/// keeps one knob: iceoryx2 formats a record exactly when `RUST_LOG` would
/// admit one at that level.
pub fn install_iceoryx2_log_bridge_at_the_engines_configured_level() {
    if iceoryx2_log::set_logger(&HOST_BRIDGE) {
        THIS_PROCESSES_ICEORYX2_LOGGER_IS_THE_BRIDGE.store(true, Ordering::Relaxed);
    } else if !THIS_PROCESSES_ICEORYX2_LOGGER_IS_THE_BRIDGE.load(Ordering::Relaxed) {
        tracing::warn!(
            "iceoryx2 already had a logger when the engine went to install its bridge, so \
             iceoryx2's records go to this process's standard error instead of into the log"
        );
    }
    iceoryx2_log::set_log_level(iceoryx2_log_level_for(LevelFilter::current()));
}

/// The iceoryx2 level that lets through exactly what `tracing_level` admits.
///
/// `OFF` has no iceoryx2 counterpart — `Fatal` is as quiet as iceoryx2 gets —
/// so a record iceoryx2 still formats there is dropped by the subscriber
/// instead.
fn iceoryx2_log_level_for(tracing_level: LevelFilter) -> LogLevel {
    match tracing_level {
        LevelFilter::TRACE => LogLevel::Trace,
        LevelFilter::DEBUG => LogLevel::Debug,
        LevelFilter::INFO => LogLevel::Info,
        LevelFilter::WARN => LogLevel::Warn,
        LevelFilter::ERROR => LogLevel::Error,
        _ => LogLevel::Fatal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every level a subscriber can admit maps onto the iceoryx2 level that
    /// lets exactly that much through.
    #[test]
    fn each_tracing_level_maps_onto_the_iceoryx2_level_admitting_the_same_records() {
        assert_eq!(iceoryx2_log_level_for(LevelFilter::TRACE), LogLevel::Trace);
        assert_eq!(iceoryx2_log_level_for(LevelFilter::DEBUG), LogLevel::Debug);
        assert_eq!(iceoryx2_log_level_for(LevelFilter::INFO), LogLevel::Info);
        assert_eq!(iceoryx2_log_level_for(LevelFilter::WARN), LogLevel::Warn);
        assert_eq!(iceoryx2_log_level_for(LevelFilter::ERROR), LogLevel::Error);
    }

    /// A subscriber admitting nothing leaves iceoryx2 at its quietest level
    /// rather than at its default, which would be the noisiest setting of the
    /// six reaching a subscriber that wants none of them.
    #[test]
    fn a_subscriber_admitting_nothing_leaves_iceoryx2_at_its_quietest_level() {
        assert_eq!(iceoryx2_log_level_for(LevelFilter::OFF), LogLevel::Fatal);
    }
}
