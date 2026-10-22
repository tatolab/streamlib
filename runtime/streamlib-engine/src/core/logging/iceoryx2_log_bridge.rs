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

/// Most bytes of an iceoryx2 record's `origin` the bridge forwards — enough for
/// the leading type name of a `from self` record, whose origin is the `Debug`
/// of the whole iceoryx2 object.
pub(crate) const ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES: usize = 128;

/// Most bytes of an iceoryx2 record's message the bridge forwards.
pub(crate) const ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES: usize = 512;

/// Zero-sized bridge implementing iceoryx2's [`Log`] trait by
/// forwarding records into the streamlib tracing pipeline, each field on one
/// line and bounded by its cap.
pub struct IceoryxLogBridge;

impl Log for IceoryxLogBridge {
    fn log(
        &self,
        log_level: LogLevel,
        origin: core::fmt::Arguments,
        formatted_message: core::fmt::Arguments,
    ) {
        let origin = Iceoryx2LogRecordFieldOnOneBoundedLine {
            field: origin,
            cap_bytes: ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES,
        };
        let message = Iceoryx2LogRecordFieldOnOneBoundedLine {
            field: formatted_message,
            cap_bytes: ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES,
        };
        // iceoryx2 aborts shortly after a `Fatal`, and tracing has no level
        // above `Error`.
        match log_level {
            LogLevel::Trace => tracing::trace!(target: "iceoryx2", origin = %origin, "{message}"),
            LogLevel::Debug => tracing::debug!(target: "iceoryx2", origin = %origin, "{message}"),
            LogLevel::Info => tracing::info!(target: "iceoryx2", origin = %origin, "{message}"),
            LogLevel::Warn => tracing::warn!(target: "iceoryx2", origin = %origin, "{message}"),
            LogLevel::Error | LogLevel::Fatal => {
                tracing::error!(target: "iceoryx2", origin = %origin, "{message}")
            }
        }
    }
}

/// What a field cut at `cap_bytes` ends with.
struct Iceoryx2LogRecordTruncationMarker {
    cap_bytes: usize,
}

impl core::fmt::Display for Iceoryx2LogRecordTruncationMarker {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "… [truncated at {} bytes]", self.cap_bytes)
    }
}

/// What a field cut at `cap_bytes` ends with, as text.
#[cfg(test)]
pub(crate) fn iceoryx2_log_record_truncation_marker(cap_bytes: usize) -> String {
    Iceoryx2LogRecordTruncationMarker { cap_bytes }.to_string()
}

/// An iceoryx2 record's field, displayed with every control character as a
/// space and cut on a character boundary at `cap_bytes`, followed by the
/// truncation marker. Formatted only when a subscriber keeps the record, and
/// never past the cap.
struct Iceoryx2LogRecordFieldOnOneBoundedLine<'a> {
    field: core::fmt::Arguments<'a>,
    cap_bytes: usize,
}

impl core::fmt::Display for Iceoryx2LogRecordFieldOnOneBoundedLine<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut bounded_formatter = Iceoryx2LogRecordFieldBoundedFormatter {
            formatter,
            bytes_written: 0,
            cap_bytes: self.cap_bytes,
            reached_the_cap: false,
        };
        let field_formatting = core::fmt::write(&mut bounded_formatter, self.field);
        if bounded_formatter.reached_the_cap {
            return core::fmt::Display::fmt(
                &Iceoryx2LogRecordTruncationMarker {
                    cap_bytes: self.cap_bytes,
                },
                formatter,
            );
        }
        field_formatting
    }
}

/// Forwards one field to `formatter` on one line, refusing any write past
/// `cap_bytes`.
struct Iceoryx2LogRecordFieldBoundedFormatter<'formatter, 'output> {
    formatter: &'formatter mut core::fmt::Formatter<'output>,
    bytes_written: usize,
    cap_bytes: usize,
    reached_the_cap: bool,
}

impl core::fmt::Write for Iceoryx2LogRecordFieldBoundedFormatter<'_, '_> {
    fn write_str(&mut self, piece: &str) -> core::fmt::Result {
        if self.reached_the_cap {
            return Err(core::fmt::Error);
        }
        for character in piece.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            if self.bytes_written + character.len_utf8() > self.cap_bytes {
                self.reached_the_cap = true;
                return Err(core::fmt::Error);
            }
            self.formatter.write_char(character)?;
            self.bytes_written += character.len_utf8();
        }
        Ok(())
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
    use std::cell::Cell;

    use serial_test::serial;
    use streamlib_runtime_client_contract::runtime_log_event::LogLevel as RuntimeLogEventLevel;

    use super::*;
    use crate::core::logging::one_stream_log_file_written_on_a_test_thread::{
        OneStreamLogFileWrittenOnThisTestThread, read_every_record_of_a_jsonl_log,
    };
    use crate::core::logging::{LoggingTunables, StreamlibLoggingConfig};

    /// A `Debug` as long as `pieces` single-byte writes, counting how many it
    /// was let make.
    struct DebugOfManyPiecesCountingItsWrites<'a> {
        pieces: usize,
        writes_made: &'a Cell<usize>,
    }

    impl core::fmt::Debug for DebugOfManyPiecesCountingItsWrites<'_> {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for _ in 0..self.pieces {
                self.writes_made.set(self.writes_made.get() + 1);
                formatter.write_str("x")?;
            }
            Ok(())
        }
    }

    fn field_as_the_bridge_forwards_it(field: core::fmt::Arguments, cap_bytes: usize) -> String {
        Iceoryx2LogRecordFieldOnOneBoundedLine { field, cap_bytes }.to_string()
    }

    #[test]
    fn a_field_within_its_cap_is_forwarded_unchanged() {
        let service = "699db7594e4b2afc49d60cb96216d8b9f8307333";
        assert_eq!(
            field_as_the_bridge_forwards_it(
                format_args!("Detected dead node in service {service}."),
                ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES,
            ),
            format!("Detected dead node in service {service}."),
        );
    }

    #[test]
    fn a_field_exactly_at_its_cap_carries_no_truncation_marker() {
        let field = "y".repeat(16);
        assert_eq!(
            field_as_the_bridge_forwards_it(format_args!("{field}"), 16),
            field,
        );
    }

    #[test]
    fn a_field_past_its_cap_is_cut_there_and_says_so() {
        let field = "z".repeat(4096);
        assert_eq!(
            field_as_the_bridge_forwards_it(format_args!("{field}"), 128),
            format!(
                "{}{}",
                "z".repeat(128),
                iceoryx2_log_record_truncation_marker(128)
            ),
        );
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let field = "aé";
        assert_eq!(
            field_as_the_bridge_forwards_it(format_args!("{field}"), 2),
            format!("a{}", iceoryx2_log_record_truncation_marker(2)),
        );
    }

    /// A `Debug` that carries on writing after a write it was refused.
    struct DebugIgnoringItsWriteErrors;

    impl core::fmt::Debug for DebugIgnoringItsWriteErrors {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let _ = formatter.write_str("abé");
            let _ = formatter.write_str("Z");
            Ok(())
        }
    }

    #[test]
    fn nothing_after_the_cut_is_forwarded_even_when_the_field_ignores_the_refusal() {
        assert_eq!(
            field_as_the_bridge_forwards_it(format_args!("{DebugIgnoringItsWriteErrors:?}"), 3),
            format!("ab{}", iceoryx2_log_record_truncation_marker(3)),
        );
    }

    #[test]
    fn a_field_spanning_lines_is_forwarded_on_one() {
        assert_eq!(
            field_as_the_bridge_forwards_it(
                format_args!("first\nsecond\r\tthird"),
                ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES,
            ),
            "first second  third",
        );
    }

    #[test]
    fn a_field_past_its_cap_is_not_formatted_beyond_it() {
        let writes_made = Cell::new(0);
        let long_debug = DebugOfManyPiecesCountingItsWrites {
            pieces: 1_000_000,
            writes_made: &writes_made,
        };
        field_as_the_bridge_forwards_it(format_args!("{long_debug:?}"), 128);
        assert_eq!(
            writes_made.get(),
            129,
            "formatting must stop at the first write past the cap"
        );
    }

    #[test]
    fn a_record_no_subscriber_keeps_is_never_formatted() {
        let writes_made = Cell::new(0);
        let long_debug = DebugOfManyPiecesCountingItsWrites {
            pieces: 4096,
            writes_made: &writes_made,
        };
        let error_only_subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_writer(std::io::sink)
            .finish();
        tracing::subscriber::with_default(error_only_subscriber, || {
            HOST_BRIDGE.log(
                LogLevel::Debug,
                format_args!("{long_debug:?}"),
                format_args!("{long_debug:?}"),
            );
        });
        assert_eq!(writes_made.get(), 0);
    }

    /// A `from self` record carrying a multi-kilobyte `Debug` origin reaches
    /// the stream's JSONL as one schema-valid line, its origin cut at the cap.
    #[test]
    #[serial]
    fn a_record_with_a_multi_kilobyte_origin_reaches_the_jsonl_bounded() {
        unsafe { std::env::set_var("RUST_LOG", "debug") };
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let logging = OneStreamLogFileWrittenOnThisTestThread::install(
            StreamlibLoggingConfig {
                service_name: "test".into(),
                pretty_log_mirror_stream: None,
                intercept_stdio: false,
                tunables: LoggingTunables::default(),
            },
            "RtestIceoryx2Bound",
            project_directory.path(),
        );

        let writes_made = Cell::new(0);
        let builder_sized_debug = DebugOfManyPiecesCountingItsWrites {
            pieces: 8 * 1024,
            writes_made: &writes_made,
        };
        HOST_BRIDGE.log(
            LogLevel::Warn,
            format_args!("{builder_sized_debug:?}"),
            format_args!(
                "Detected dead node in service 699db.\nTrying to cleanup stale resources."
            ),
        );

        let path = logging.jsonl_log_path();
        logging.finish();
        let records = read_every_record_of_a_jsonl_log(&path);
        let record = records
            .iter()
            .find(|record| record.target == "iceoryx2")
            .expect("the bridged record reaches the stream's JSONL");

        assert_eq!(record.level, RuntimeLogEventLevel::Warn);
        assert_eq!(
            record.message,
            "Detected dead node in service 699db. Trying to cleanup stale resources."
        );
        assert_eq!(
            record
                .attrs
                .get("origin")
                .and_then(|origin| origin.as_str()),
            Some(
                format!(
                    "{}{}",
                    "x".repeat(ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES),
                    iceoryx2_log_record_truncation_marker(ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES)
                )
                .as_str()
            ),
        );
    }

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
