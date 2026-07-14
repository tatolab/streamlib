// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-side `iceoryx2_log::Log` that forwards every iceoryx2 log
//! record to the host via [`crate::plugin::HostCallbacks::iceoryx_log_emit`].
//!
//! iceoryx2 records emitted from cdylib code (e.g. `Iceoryx2Node`
//! operations inside a processor's `process()`) reach the host's tracing
//! pipeline identically to records emitted from host code.

use iceoryx2_log::Log;
use iceoryx2_log::LogLevel;
use streamlib_plugin_abi::HostLogLevel;

use super::host_callbacks;

/// Zero-sized forwarder. Installed via [`iceoryx2_log::set_logger`] from
/// `install_for_self`. The `Log` trait's `log(level, origin,
/// formatted_message)` shape carries `core::fmt::Arguments` for both
/// origin and message; we render them to owned `String`s before crossing
/// the FFI boundary because `Arguments` is not FFI-safe.
pub struct CdylibIceoryx2LogForwarder;

impl Log for CdylibIceoryx2LogForwarder {
    fn log(
        &self,
        level: LogLevel,
        origin: core::fmt::Arguments,
        formatted_message: core::fmt::Arguments,
    ) {
        let Some(cbs) = host_callbacks() else { return };

        let host_level = match level {
            LogLevel::Trace => HostLogLevel::Trace,
            LogLevel::Debug => HostLogLevel::Debug,
            LogLevel::Info => HostLogLevel::Info,
            LogLevel::Warn => HostLogLevel::Warn,
            LogLevel::Error | LogLevel::Fatal => HostLogLevel::Error,
        };

        let origin_string = format!("{}", origin);
        let message_string = format!("{}", formatted_message);

        unsafe {
            (cbs.iceoryx_log_emit)(
                cbs.host,
                host_level,
                origin_string.as_ptr(),
                origin_string.len(),
                message_string.as_ptr(),
                message_string.len(),
            );
        }
    }
}

/// Process-wide forwarder value. Installed via [`iceoryx2_log::set_logger`]
/// which requires a `&'static dyn Log`.
pub static FORWARDER: CdylibIceoryx2LogForwarder = CdylibIceoryx2LogForwarder;

/// Install [`FORWARDER`] as this DSO's iceoryx2 log sink. Called by
/// `install_host_services` after the callback table is cached.
pub fn install_for_self() {
    let _ = iceoryx2_log::set_logger(&FORWARDER);
    // Raise the cdylib's iceoryx2-log level to Trace so the forwarder sees
    // every record; the host's tracing pipeline applies its own EnvFilter
    // at emit time.
    iceoryx2_log::set_log_level(iceoryx2_log::LogLevel::Trace);
}
