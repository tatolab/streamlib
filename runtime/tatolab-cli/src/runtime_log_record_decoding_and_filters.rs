// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A runtime log record as `tatolab logs` and `dev` receive it from the runtime: decoded into
//! the record the runtime wrote, and narrowed by the verb's filters.

use std::io::Write;

use serde::Deserialize;
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, RuntimeLogEvent, Source};

/// The `--processor` / `--pipeline` / `--rhi` / `--level` / `--source` / `--intercepted-only`
/// narrowing applied to each record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeLogRecordFilters {
    /// Only records from this processor id.
    pub(crate) processor_id: Option<String>,
    /// Only records from this pipeline id.
    pub(crate) pipeline_id: Option<String>,
    /// Only records carrying an `rhi_op`.
    pub(crate) rhi_operations_only: bool,
    /// Only records at this severity or above.
    pub(crate) minimum_level: Option<LogLevel>,
    /// Only records emitted by this runtime language.
    pub(crate) source: Option<Source>,
    /// Only records an interceptor captured.
    pub(crate) intercepted_only: bool,
}

impl RuntimeLogRecordFilters {
    /// Whether `event` survives every filter that is set.
    pub(crate) fn admits(&self, event: &RuntimeLogEvent) -> bool {
        if let Some(processor_id) = &self.processor_id
            && event.processor_id.as_ref() != Some(processor_id)
        {
            return false;
        }
        if let Some(pipeline_id) = &self.pipeline_id
            && event.pipeline_id.as_ref() != Some(pipeline_id)
        {
            return false;
        }
        if self.rhi_operations_only && event.rhi_op.is_none() {
            return false;
        }
        if let Some(minimum_level) = self.minimum_level
            && event.level < minimum_level
        {
            return false;
        }
        if let Some(source) = self.source
            && event.source != source
        {
            return false;
        }
        if self.intercepted_only && !event.intercepted {
            return false;
        }
        true
    }
}

/// The warning a JSON value that is not an object earns in place of a record.
const NOT_A_RECORD_OBJECT_WARNING: &str =
    "warning: skipping JSONL line that is not a record object";

/// The warning an object whose fields are not a record's earns.
const RECORD_SCHEMA_MISMATCH_WARNING: &str =
    "warning: skipping JSONL line whose fields do not match the record schema";

/// A record the runtime handed over already parsed, or `None` — with the warning the same JSONL
/// line would earn on `warning_output` — when it is not one.
pub(crate) fn decode_runtime_log_record_value(
    record: &serde_json::Value,
    warning_output: &mut dyn Write,
) -> Option<RuntimeLogEvent> {
    if let Ok(event) = RuntimeLogEvent::deserialize(record) {
        return Some(event);
    }
    let warning = if record.is_object() {
        RECORD_SCHEMA_MISMATCH_WARNING
    } else {
        NOT_A_RECORD_OBJECT_WARNING
    };
    let _ = writeln!(warning_output, "{warning}");
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use streamlib_runtime_client_contract::runtime_log_event_pretty_rendering::format_event_pretty;

    use super::*;
    use crate::runtime_log_line_fixtures::a_log_record;

    /// `records` decoded and narrowed by `record_filters`, each rendered, and the notes written.
    fn rendered_records_of(
        records: &[serde_json::Value],
        record_filters: &RuntimeLogRecordFilters,
    ) -> (Vec<String>, String) {
        let mut note_output = Vec::new();
        let rendered_records = records
            .iter()
            .filter_map(|record| decode_runtime_log_record_value(record, &mut note_output))
            .filter(|event| record_filters.admits(event))
            .map(|event| {
                let mut rendered_record = String::new();
                format_event_pretty(&event, &mut rendered_record);
                rendered_record
            })
            .collect();
        (rendered_records, String::from_utf8(note_output).unwrap())
    }

    fn messages_of(rendered_records: &[String]) -> Vec<String> {
        rendered_records
            .iter()
            .map(|rendered_record| {
                rendered_record
                    .trim_end()
                    .rsplit_once(" — ")
                    .unwrap()
                    .1
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn a_record_renders_exactly_as_the_runtime_mirrored_it() {
        let (rendered_records, notes) = rendered_records_of(
            &[a_log_record(json!({
                "target": "streamlib_media_builtins::camera_source",
                "pipeline_id": "pipe",
                "processor_id": "proc",
                "rhi_op": "acquire_texture",
                "attrs": {
                    "width": 1920,
                    "origin": "Settings::defaults()",
                    "device": "Logitech Café",
                    "v": 1e-5,
                },
            }))],
            &RuntimeLogRecordFilters::default(),
        );

        assert_eq!(
            rendered_records,
            [
                "21:04:27.573 [ INFO] [Rabc/rust] streamlib_media_builtins::camera_source — \
              Creating Runner pipeline_id=pipe processor_id=proc rhi_op=acquire_texture \
              device=\"Logitech Café\" origin=\"Settings::defaults()\" v=0.00001 width=1920\n"
            ]
        );
        assert_eq!(notes, "");
    }

    #[test]
    fn each_filter_narrows_to_the_records_it_names() {
        let records = [
            a_log_record(json!({"message": "info-rust"})),
            a_log_record(json!({"message": "warn-python", "level": "warn", "source": "python"})),
            a_log_record(
                json!({"message": "rhi-op", "rhi_op": "acquire", "processor_id": "proc-1"}),
            ),
            a_log_record(
                json!({"message": "intercepted", "intercepted": true, "pipeline_id": "pipe-1"}),
            ),
        ];

        for (record_filters, expected_messages) in [
            (
                RuntimeLogRecordFilters::default(),
                &["info-rust", "warn-python", "rhi-op", "intercepted"][..],
            ),
            (
                RuntimeLogRecordFilters {
                    minimum_level: Some(LogLevel::Warn),
                    ..Default::default()
                },
                &["warn-python"],
            ),
            (
                RuntimeLogRecordFilters {
                    source: Some(Source::Python),
                    ..Default::default()
                },
                &["warn-python"],
            ),
            (
                RuntimeLogRecordFilters {
                    rhi_operations_only: true,
                    ..Default::default()
                },
                &["rhi-op"],
            ),
            (
                RuntimeLogRecordFilters {
                    intercepted_only: true,
                    ..Default::default()
                },
                &["intercepted"],
            ),
            (
                RuntimeLogRecordFilters {
                    processor_id: Some("proc-1".to_owned()),
                    ..Default::default()
                },
                &["rhi-op"],
            ),
            (
                RuntimeLogRecordFilters {
                    pipeline_id: Some("pipe-1".to_owned()),
                    ..Default::default()
                },
                &["intercepted"],
            ),
            (
                RuntimeLogRecordFilters {
                    processor_id: Some("absent".to_owned()),
                    ..Default::default()
                },
                &[],
            ),
        ] {
            let (rendered_records, _) = rendered_records_of(&records, &record_filters);

            assert_eq!(
                messages_of(&rendered_records),
                expected_messages,
                "{record_filters:?}"
            );
        }
    }

    #[test]
    fn a_level_floor_admits_its_own_level_and_every_more_severe_one() {
        let levels_least_severe_first = ["trace", "debug", "info", "warn", "error"];
        let records = levels_least_severe_first
            .map(|level| a_log_record(json!({"level": level, "message": level})));

        for (floor_index, minimum_level) in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ]
        .into_iter()
        .enumerate()
        {
            let (rendered_records, _) = rendered_records_of(
                &records,
                &RuntimeLogRecordFilters {
                    minimum_level: Some(minimum_level),
                    ..Default::default()
                },
            );

            assert_eq!(
                messages_of(&rendered_records),
                levels_least_severe_first[floor_index..],
                "{minimum_level:?}"
            );
        }
    }

    /// Each is JSON but not shaped like a record; an unknown level is one of them, since the
    /// record's level is the schema's enum.
    #[test]
    fn a_schema_invalid_record_is_skipped_with_a_warning() {
        const SCHEMA_WARNING: &str =
            "warning: skipping JSONL line whose fields do not match the record schema\n";
        for (schema_invalid_record, expected_warning) in [
            (json!({"host_ts": null, "level": "info"}), SCHEMA_WARNING),
            (json!({"host_ts": 1, "level": 7}), SCHEMA_WARNING),
            (
                a_log_record(json!({"attrs": "not a mapping"})),
                SCHEMA_WARNING,
            ),
            (a_log_record(json!({"level": "fatal"})), SCHEMA_WARNING),
            (
                json!(["not", "an", "object"]),
                "warning: skipping JSONL line that is not a record object\n",
            ),
        ] {
            let (rendered_records, notes) = rendered_records_of(
                &[
                    schema_invalid_record.clone(),
                    a_log_record(json!({"message": "good"})),
                ],
                &RuntimeLogRecordFilters::default(),
            );

            assert_eq!(
                messages_of(&rendered_records),
                ["good"],
                "{schema_invalid_record}"
            );
            assert_eq!(notes, expected_warning, "{schema_invalid_record}");
        }
    }
}
