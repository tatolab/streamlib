// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bridge iceoryx2-log records into the streamlib tracing pipeline.
//!
//! `iceoryx2` exposes its own logging trait via `iceoryx2-log`'s
//! [`Log`] interface plus a one-shot global `set_logger` that takes a
//! `&'static dyn Log`. Without a bridge, iceoryx2's internal log
//! records go to its default stderr logger and bypass the streamlib
//! JSONL pipeline entirely. With this bridge, `Runner::new` calls
//! `install_iceoryx2_log_bridge()` once on first construction,
//! installing [`HOST_BRIDGE`] as `iceoryx2`'s process-wide logger.

use iceoryx2_log::Log;
use iceoryx2_log::LogLevel;

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

/// Install [`HOST_BRIDGE`] as iceoryx2's process-wide
/// logger. Idempotent — `iceoryx2_log::set_logger` is `Once`-guarded
/// and returns false on subsequent calls, which we treat as success.
pub fn install_iceoryx2_log_bridge() {
    let _ = iceoryx2_log::set_logger(&HOST_BRIDGE);
}
