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

use std::fmt::Write as _;
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
        let origin = iceoryx2_log_record_field_on_one_bounded_line(
            origin,
            ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES,
        );
        let message = iceoryx2_log_record_field_on_one_bounded_line(
            formatted_message,
            ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES,
        );
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
pub(crate) fn iceoryx2_log_record_truncation_marker(cap_bytes: usize) -> String {
    format!("… [truncated at {cap_bytes} bytes]")
}

/// `field` rendered with every control character as a space, cut on a
/// character boundary at `cap_bytes` and then carrying the truncation marker.
fn iceoryx2_log_record_field_on_one_bounded_line(
    field: core::fmt::Arguments,
    cap_bytes: usize,
) -> String {
    let mut rendering = Iceoryx2LogRecordFieldRenderedOnOneBoundedLine {
        rendered: String::new(),
        cap_bytes,
        reached_the_cap: false,
    };
    // An error here is the rendering refusing past its cap, which stops the
    // field's formatting there; the rest of the field is never formatted.
    let _ = rendering.write_fmt(field);
    if rendering.reached_the_cap {
        rendering
            .rendered
            .push_str(&iceoryx2_log_record_truncation_marker(cap_bytes));
    }
    rendering.rendered
}

/// A field being rendered on one line, refusing any write past its cap.
struct Iceoryx2LogRecordFieldRenderedOnOneBoundedLine {
    rendered: String,
    cap_bytes: usize,
    reached_the_cap: bool,
}

impl core::fmt::Write for Iceoryx2LogRecordFieldRenderedOnOneBoundedLine {
    fn write_str(&mut self, piece: &str) -> core::fmt::Result {
        for character in piece.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            if self.rendered.len() + character.len_utf8() > self.cap_bytes {
                self.reached_the_cap = true;
                return Err(core::fmt::Error);
            }
            self.rendered.push(character);
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

    #[test]
    fn a_field_within_its_cap_is_forwarded_unchanged() {
        let service = "699db7594e4b2afc49d60cb96216d8b9f8307333";
        assert_eq!(
            iceoryx2_log_record_field_on_one_bounded_line(
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
            iceoryx2_log_record_field_on_one_bounded_line(format_args!("{field}"), 16),
            field,
        );
    }

    #[test]
    fn a_field_past_its_cap_is_cut_there_and_says_so() {
        let field = "z".repeat(4096);
        assert_eq!(
            iceoryx2_log_record_field_on_one_bounded_line(format_args!("{field}"), 128),
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
            iceoryx2_log_record_field_on_one_bounded_line(format_args!("{field}"), 2),
            format!("a{}", iceoryx2_log_record_truncation_marker(2)),
        );
    }

    #[test]
    fn a_field_spanning_lines_is_forwarded_on_one() {
        assert_eq!(
            iceoryx2_log_record_field_on_one_bounded_line(
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
        iceoryx2_log_record_field_on_one_bounded_line(format_args!("{long_debug:?}"), 128);
        assert_eq!(
            writes_made.get(),
            129,
            "formatting must stop at the first write past the cap"
        );
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
        let jsonl = std::fs::read_to_string(&path).unwrap();
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
        let bridged_line = jsonl
            .lines()
            .find(|line| line.contains("\"target\":\"iceoryx2\""))
            .expect("the bridged record is one line");
        assert!(
            bridged_line.len() < 1024,
            "the bridged record's line is {} bytes",
            bridged_line.len()
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
