// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's log records as the runtime holds them: read through the local API's `logs`
//! tool by sequence number, never by time, and rendered as the runtime mirrors them.

use std::io::Write;

use serde::Deserialize;
use streamlib_runtime_client_contract::runtime_log_event_pretty_rendering::format_event_pretty;

use crate::runtime_log_files_reader::{RuntimeLogRecordFilters, decode_runtime_log_line};

/// The local API tool that pages a loaded stream's log records by sequence number.
pub(crate) const LOGS_TOOL_NAME: &str = "logs";

/// How long a reader following a stream's records waits after a page that brought nothing.
pub(crate) const STREAM_LOG_RECORDS_FOLLOW_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

/// One answer of `logs {stream, after}`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct StreamLogRecordsPage {
    /// The records after `after`, oldest first.
    pub(crate) records: Vec<NumberedStreamLogRecord>,
    /// The `after` the next call passes to read on from this page.
    pub(crate) next_after: u64,
    /// How many records after `after` the runtime dropped from its history before this read.
    pub(crate) records_no_longer_held: u64,
}

/// One record of a stream's log, numbered by the runtime from 1 per loaded stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct NumberedStreamLogRecord {
    /// The record's place in its stream's log.
    pub(crate) sequence: u64,
    /// The JSONL record, as the stream's log file holds it.
    pub(crate) record: serde_json::Value,
}

/// `logs`' arguments for the records of `stream` after the sequence number `after`.
pub(crate) fn logs_tool_arguments_after(
    stream: &str,
    after: u64,
) -> serde_json::Map<String, serde_json::Value> {
    let mut logs_arguments = serde_json::Map::new();
    logs_arguments.insert("stream".to_owned(), stream.into());
    logs_arguments.insert("after".to_owned(), after.into());
    logs_arguments
}

/// The page `logs_tool_result_text` carries, or why it is not one.
pub(crate) fn stream_log_records_page_from(
    logs_tool_result_text: &str,
) -> Result<StreamLogRecordsPage, String> {
    serde_json::from_str(logs_tool_result_text).map_err(|parse_failure| {
        format!("logs answered something other than a page of records: {parse_failure}")
    })
}

/// `stream_log_records_page`'s records that pass `record_filters`, each rendered with its
/// newline; a record that is not one, and records the runtime no longer held, are noted on
/// `note_output`.
pub(crate) fn render_stream_log_records_page(
    stream: &str,
    stream_log_records_page: &StreamLogRecordsPage,
    record_filters: &RuntimeLogRecordFilters,
    note_output: &mut dyn Write,
) -> String {
    if stream_log_records_page.records_no_longer_held > 0 {
        let _ = writeln!(
            note_output,
            "note: {} records of `{stream}` were no longer held by the runtime when they were read",
            stream_log_records_page.records_no_longer_held
        );
    }
    let mut rendered_records = String::new();
    for numbered_record in &stream_log_records_page.records {
        let record_line = numbered_record.record.to_string();
        if let Some(event) = decode_runtime_log_line(record_line.as_bytes(), note_output)
            && record_filters.admits(&event)
        {
            format_event_pretty(&event, &mut rendered_records);
        }
    }
    rendered_records
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use streamlib_runtime_client_contract::runtime_log_event::LogLevel;

    use super::*;
    use crate::runtime_log_line_fixtures::a_log_record;

    fn a_page_of(
        records: &[serde_json::Value],
        records_no_longer_held: u64,
    ) -> StreamLogRecordsPage {
        stream_log_records_page_from(
            &json!({
                "stream": "camera",
                "records": records
                    .iter()
                    .enumerate()
                    .map(|(record_index, record)| json!({"sequence": record_index + 1, "record": record}))
                    .collect::<Vec<_>>(),
                "next_after": records.len(),
                "records_no_longer_held": records_no_longer_held,
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn logs_is_asked_for_the_stream_and_the_records_after_a_sequence_number() {
        assert_eq!(
            serde_json::Value::Object(logs_tool_arguments_after("camera", 41)),
            json!({"stream": "camera", "after": 41})
        );
    }

    #[test]
    fn each_record_renders_as_the_runtime_mirrors_it() {
        let mut notes = Vec::new();

        let rendered = render_stream_log_records_page(
            "camera",
            &a_page_of(
                &[
                    a_log_record(json!({"message": "first"})),
                    a_log_record(json!({"message": "second", "level": "warn"})),
                ],
                0,
            ),
            &RuntimeLogRecordFilters::default(),
            &mut notes,
        );

        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — first\n\
             21:04:27.573 [ WARN] [Rabc/rust] tatolabd — second\n"
        );
        assert_eq!(notes, b"");
    }

    #[test]
    fn the_filters_narrow_the_rendered_records() {
        let rendered = render_stream_log_records_page(
            "camera",
            &a_page_of(
                &[
                    a_log_record(json!({"message": "quiet"})),
                    a_log_record(json!({"message": "loud", "level": "error"})),
                ],
                0,
            ),
            &RuntimeLogRecordFilters {
                minimum_level: Some(LogLevel::Warn),
                ..RuntimeLogRecordFilters::default()
            },
            &mut Vec::new(),
        );

        assert_eq!(
            rendered,
            "21:04:27.573 [ERROR] [Rabc/rust] tatolabd — loud\n"
        );
    }

    #[test]
    fn records_the_runtime_no_longer_held_and_a_record_that_is_not_one_are_noted() {
        let mut notes = Vec::new();

        let rendered = render_stream_log_records_page(
            "camera",
            &a_page_of(&[json!({"not": "a record"})], 7),
            &RuntimeLogRecordFilters::default(),
            &mut notes,
        );

        assert_eq!(rendered, "");
        assert_eq!(
            String::from_utf8(notes).unwrap(),
            "note: 7 records of `camera` were no longer held by the runtime when they were read\n\
             warning: skipping JSONL line whose fields do not match the record schema\n"
        );
    }

    #[test]
    fn an_answer_that_is_not_a_page_is_refused_naming_logs() {
        let refusal = stream_log_records_page_from("[]").unwrap_err();

        assert!(
            refusal.starts_with("logs answered something other than a page of records: "),
            "{refusal}"
        );
    }
}
