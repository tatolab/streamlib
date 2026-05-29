// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bridge iceoryx2-log records into the streamlib tracing pipeline.
//!
//! `iceoryx2` exposes its own logging trait via `iceoryx2-log`'s
//! [`Log`] interface plus a one-shot global `set_logger` that takes a
//! `&'static dyn Log`. Without a bridge, iceoryx2's internal log
//! records go to its default stderr logger and bypass the streamlib
//! JSONL pipeline entirely. With this bridge:
//!
//! - Host-side: `Runner::new` calls `install_iceoryx2_log_bridge()`
//!   once on first construction, installing [`HOST_BRIDGE`] as
//!   `iceoryx2`'s process-wide logger.
//! - Plugin cdylib-side: the cdylib's `STREAMLIB_PLUGIN` register
//!   callback receives the host's `&'static dyn Log` pointer via
//!   [`crate::core::plugin::HostServices::iceoryx2_logger`] and
//!   installs it in the cdylib's `iceoryx2-log` static. The host and
//!   each plugin have their own `iceoryx2-log` static; both end up
//!   pointing at the same bridge value living in host memory.
//!
//! Plugin ABI safety: the host and the plugin link the same
//! `iceoryx2-log-types` version (workspace pin), so the
//! `&'static dyn Log` trait object's vtable is layout-compatible
//! on both sides. Logging through the pointer dispatches through the
//! host's vtable, which calls back into the host's tracing pipeline.

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

/// Process-wide bridge value. Lives in `.rodata` (zero-sized) per
/// loaded artifact; the host's copy is the one shared with plugin cdylibs via
/// [`crate::core::plugin::HostServices::iceoryx2_logger`]. Both
/// host's and cdylib's copies impl `Log` against the same workspace-
/// pinned `iceoryx2-log-types::Log` vtable.
pub static HOST_BRIDGE: IceoryxLogBridge = IceoryxLogBridge;

/// Install [`HOST_BRIDGE`] as this artifact's iceoryx2 process-wide
/// logger. Idempotent — `iceoryx2_log::set_logger` is `Once`-guarded
/// and returns false on subsequent calls, which we treat as success.
pub fn install_iceoryx2_log_bridge_for_self() {
    let _ = iceoryx2_log::set_logger(&HOST_BRIDGE);
}

/// Install a foreign `&'static dyn Log` into this plugin's iceoryx2
/// process-wide logger. Used by plugin cdylibs receiving the host's
/// bridge pointer through `HostServices`.
///
/// # Safety
///
/// `logger` must remain valid for the lifetime of this plugin. The
/// host's [`HOST_BRIDGE`] is `'static` by construction, so passing
/// `&HOST_BRIDGE` from the host satisfies this.
pub unsafe fn install_foreign_iceoryx2_logger(logger: &'static dyn Log) {
    let _ = iceoryx2_log::set_logger(logger);
}
