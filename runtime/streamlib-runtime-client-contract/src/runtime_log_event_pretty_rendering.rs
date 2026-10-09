// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The human-readable layout of one [`RuntimeLogEvent`]: what the runtime's pretty
//! mirror writes live, and what the native `tatolab logs` renders a replayed record
//! with, so one record reads the same both ways.

use crate::runtime_log_event::{LogLevel, RuntimeLogEvent};

/// Append `event` to `out` in the runtime's human-readable layout, newline included.
pub fn format_event_pretty(event: &RuntimeLogEvent, out: &mut String) {
    use std::fmt::Write;
    let level = match event.level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => " INFO",
        LogLevel::Warn => " WARN",
        LogLevel::Error => "ERROR",
    };
    let _ = write!(
        out,
        "{} [{:>5}] [",
        format_ns_timestamp(event.host_ts),
        level
    );
    if !event.runtime_id.is_empty() {
        let _ = write!(out, "{}/", event.runtime_id);
    }
    if let Some(stream) = &event.stream {
        let _ = write!(out, "{}/", stream);
    }
    let _ = write!(
        out,
        "{}] {} — {}",
        event.source.as_str(),
        event.target,
        event.message,
    );
    if let Some(p) = &event.pipeline_id {
        let _ = write!(out, " pipeline_id={}", p);
    }
    if let Some(p) = &event.processor_id {
        let _ = write!(out, " processor_id={}", p);
    }
    if let Some(r) = &event.rhi_op {
        let _ = write!(out, " rhi_op={}", r);
    }
    for (k, v) in &event.attrs {
        let _ = write!(out, " {}={}", k, v);
    }
    out.push('\n');
}

/// `host_ts` nanoseconds as the compact UTC `HH:MM:SS.mmm` the pretty layout leads with.
pub fn format_ns_timestamp(ns: u64) -> String {
    // Enough for humans tailing logs. Full authoritative timestamp remains in
    // the JSONL as `host_ts`.
    let secs_total = ns / 1_000_000_000;
    let ms = (ns % 1_000_000_000) / 1_000_000;
    let hh = (secs_total / 3600) % 24;
    let mm = (secs_total / 60) % 60;
    let ss = secs_total % 60;
    format!("{:02}:{:02}:{:02}.{:03}", hh, mm, ss, ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_log_event::Source;

    fn a_rust_info_record_of_runtime_rabc(target: &str, message: &str) -> RuntimeLogEvent {
        RuntimeLogEvent {
            schema_version: 1,
            host_ts: 1_786_136_667_573_387_556,
            runtime_id: "Rabc".to_string(),
            stream: None,
            source: Source::Rust,
            level: LogLevel::Info,
            message: message.to_string(),
            target: target.to_string(),
            pipeline_id: None,
            processor_id: None,
            rhi_op: None,
            source_ts: None,
            source_seq: None,
            intercepted: false,
            channel: None,
            attrs: Default::default(),
        }
    }

    /// A replayed record must render to the same bytes the runtime's mirror wrote
    /// live, or one record reads as two different records.
    ///
    /// The `target` names a module outside the engine: `check-boundaries` reads an
    /// engine module path in this crate's source as an engine import.
    #[test]
    fn the_pretty_rendering_matches_the_golden() {
        let event = a_rust_info_record_of_runtime_rabc("tatolabd", "Creating Runner");

        let mut rendered = String::new();
        format_event_pretty(&event, &mut rendered);

        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — Creating Runner\n"
        );
    }

    #[test]
    fn a_records_ids_then_its_attrs_in_key_order_follow_the_message() {
        let mut event = a_rust_info_record_of_runtime_rabc(
            "streamlib_media_builtins::camera_source",
            "frame captured",
        );
        event.level = LogLevel::Warn;
        event.pipeline_id = Some("pl-42".to_string());
        event.processor_id = Some("camera-1".to_string());
        event.rhi_op = Some("acquire_texture".to_string());
        event
            .attrs
            .insert("frames".to_string(), serde_json::Value::from(3));
        event
            .attrs
            .insert("device".to_string(), serde_json::Value::from("/dev/video0"));

        let mut rendered = String::new();
        format_event_pretty(&event, &mut rendered);

        assert_eq!(
            rendered,
            "21:04:27.573 [ WARN] [Rabc/rust] streamlib_media_builtins::camera_source — \
             frame captured pipeline_id=pl-42 processor_id=camera-1 rhi_op=acquire_texture \
             device=\"/dev/video0\" frames=3\n"
        );
    }

    #[test]
    fn a_streams_record_names_its_stream_between_the_runtime_and_the_source() {
        let mut event = a_rust_info_record_of_runtime_rabc("tatolabd", "Creating Runner");
        event.stream = Some("main".to_string());

        let mut rendered = String::new();
        format_event_pretty(&event, &mut rendered);

        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [Rabc/main/rust] tatolabd — Creating Runner\n"
        );
    }

    #[test]
    fn a_record_no_runtime_claimed_renders_only_its_source() {
        let mut event = a_rust_info_record_of_runtime_rabc("tatolabd", "Creating Runner");
        event.runtime_id = String::new();

        let mut rendered = String::new();
        format_event_pretty(&event, &mut rendered);

        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [rust] tatolabd — Creating Runner\n"
        );
    }

    #[test]
    fn a_timestamp_renders_as_the_utc_time_of_day_to_the_millisecond() {
        assert_eq!(format_ns_timestamp(0), "00:00:00.000");
        assert_eq!(
            format_ns_timestamp(1_786_136_667_573_387_556),
            "21:04:27.573"
        );
        assert_eq!(format_ns_timestamp(86_399_999_999_999), "23:59:59.999");
    }
}
