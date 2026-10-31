// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's log records as the runtime reads them from the project's log on disk: paged
//! through the local API's `logs` tool by an opaque cursor, and rendered as the runtime mirrors
//! them.

use std::io::Write;
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::LogsToolResult;
use streamlib_runtime_client_contract::runtime_log_event_pretty_rendering::format_event_pretty;

use crate::runtime_log_record_decoding_and_filters::{
    RuntimeLogRecordFilters, decode_runtime_log_record_value,
};

/// The local API tool that pages a project's log records by cursor.
pub(crate) const LOGS_TOOL_NAME: &str = "logs";

/// The local API tool a runtime describes itself with, its own log's folder among it.
pub(crate) const DESCRIBE_RUNTIME_TOOL_NAME: &str = "describe_runtime";

/// How long a reader following a project's records waits after a page that brought nothing.
pub(crate) const PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

/// Which of a project's log records a `logs` call reads: the project's, narrowed to one stream
/// and one stream action when they are given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectLogRecordsAsked<'asked> {
    pub(crate) project_directory: &'asked Path,
    pub(crate) stream: Option<&'asked str>,
    pub(crate) action: Option<&'asked str>,
}

impl ProjectLogRecordsAsked<'_> {
    /// `logs`' arguments for these records after the cursor `after`, from the oldest held when
    /// `None`.
    pub(crate) fn logs_tool_arguments_after(
        &self,
        after: Option<&str>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut logs_arguments = serde_json::Map::new();
        logs_arguments.insert(
            "project_directory".to_owned(),
            self.project_directory.display().to_string().into(),
        );
        for (argument_name, argument_value) in [
            ("stream", self.stream),
            ("action", self.action),
            ("after", after),
        ] {
            if let Some(argument_value) = argument_value {
                logs_arguments.insert(argument_name.to_owned(), argument_value.into());
            }
        }
        logs_arguments
    }
}

/// The page `logs_tool_result_text` carries, or why it is not one.
pub(crate) fn project_log_records_page_from(
    logs_tool_result_text: &str,
) -> Result<LogsToolResult, String> {
    serde_json::from_str(logs_tool_result_text).map_err(|parse_failure| {
        format!("logs answered something other than a page of records: {parse_failure}")
    })
}

/// `project_log_records_page`'s records that pass `record_filters`, each rendered with its
/// newline; a record that is not one, and records retention deleted before they were read, are
/// noted on `note_output`.
pub(crate) fn render_project_log_records_page(
    project_log_records_page: &LogsToolResult,
    record_filters: &RuntimeLogRecordFilters,
    each_record_labelled_by_its_projects_folder_name: bool,
    note_output: &mut dyn Write,
) -> String {
    let projects_folder_name = project_log_records_page
        .project_directory
        .file_name()
        .map(|folder_name| folder_name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if project_log_records_page.records_no_longer_held {
        let _ = writeln!(
            note_output,
            "note: records of {} were deleted by retention before they were read; reading on \
             from the oldest held",
            project_log_records_page.project_directory.display()
        );
    }
    let mut rendered_records = String::new();
    for record in &project_log_records_page.records {
        if let Some(event) = decode_runtime_log_record_value(record, note_output)
            && record_filters.admits(&event)
        {
            if each_record_labelled_by_its_projects_folder_name {
                rendered_records.push_str(&projects_folder_name);
                rendered_records.push(' ');
            }
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

    fn a_page_of(records: &[serde_json::Value], records_no_longer_held: bool) -> LogsToolResult {
        project_log_records_page_from(
            &json!({
                "project_directory": "/srv/desk",
                "records": records,
                "next_after": format!("1:{}", records.len()),
                "records_no_longer_held": records_no_longer_held,
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn logs_is_asked_for_the_project_and_only_the_narrowing_given() {
        let every_record = ProjectLogRecordsAsked {
            project_directory: Path::new("/srv/desk"),
            stream: None,
            action: None,
        };
        assert_eq!(
            serde_json::Value::Object(every_record.logs_tool_arguments_after(None)),
            json!({"project_directory": "/srv/desk"})
        );
        let one_stream_action = ProjectLogRecordsAsked {
            stream: Some("camera-0a1b"),
            action: Some("a7"),
            ..every_record
        };
        assert_eq!(
            serde_json::Value::Object(one_stream_action.logs_tool_arguments_after(Some("2:41"))),
            json!({
                "project_directory": "/srv/desk",
                "stream": "camera-0a1b",
                "action": "a7",
                "after": "2:41",
            })
        );
    }

    #[test]
    fn each_record_renders_as_the_runtime_mirrors_it() {
        let mut notes = Vec::new();
        let rendered = render_project_log_records_page(
            &a_page_of(
                &[
                    a_log_record(json!({"message": "first"})),
                    a_log_record(json!({"message": "second", "level": "warn"})),
                ],
                false,
            ),
            &RuntimeLogRecordFilters::default(),
            false,
            &mut notes,
        );
        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — first\n\
             21:04:27.573 [ WARN] [Rabc/rust] tatolabd — second\n"
        );
        assert_eq!(notes, b"");
        assert_eq!(
            render_project_log_records_page(
                &a_page_of(&[a_log_record(json!({"message": "first"}))], false),
                &RuntimeLogRecordFilters::default(),
                true,
                &mut Vec::new(),
            ),
            "desk 21:04:27.573 [ INFO] [Rabc/rust] tatolabd — first\n",
            "a record labelled by its project's folder name"
        );
    }

    #[test]
    fn the_filters_narrow_the_rendered_records() {
        let rendered = render_project_log_records_page(
            &a_page_of(
                &[
                    a_log_record(json!({"message": "quiet"})),
                    a_log_record(json!({"message": "loud", "level": "error"})),
                ],
                false,
            ),
            &RuntimeLogRecordFilters {
                minimum_level: Some(LogLevel::Warn),
                ..RuntimeLogRecordFilters::default()
            },
            false,
            &mut Vec::new(),
        );
        assert_eq!(
            rendered,
            "21:04:27.573 [ERROR] [Rabc/rust] tatolabd — loud\n"
        );
    }

    #[test]
    fn records_retention_deleted_and_a_record_that_is_not_one_are_noted() {
        let mut notes = Vec::new();
        let rendered = render_project_log_records_page(
            &a_page_of(&[json!({"not": "a record"})], true),
            &RuntimeLogRecordFilters::default(),
            false,
            &mut notes,
        );
        assert_eq!(rendered, "");
        assert_eq!(
            String::from_utf8(notes).unwrap(),
            "note: records of /srv/desk were deleted by retention before they were read; \
             reading on from the oldest held\n\
             warning: skipping JSONL line whose fields do not match the record schema\n"
        );
    }

    #[test]
    fn a_record_that_is_not_an_object_is_noted_as_one_and_the_page_reads_on() {
        let mut notes = Vec::new();
        let rendered = render_project_log_records_page(
            &a_page_of(
                &[
                    json!("a bare string"),
                    a_log_record(json!({"message": "after"})),
                ],
                false,
            ),
            &RuntimeLogRecordFilters::default(),
            false,
            &mut notes,
        );
        assert_eq!(
            rendered,
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — after\n"
        );
        assert_eq!(
            String::from_utf8(notes).unwrap(),
            "warning: skipping JSONL line that is not a record object\n"
        );
    }

    #[test]
    fn an_answer_that_is_not_a_page_is_refused_naming_logs() {
        let refusal = project_log_records_page_from("[]").unwrap_err();
        assert!(
            refusal.starts_with("logs answered something other than a page of records: "),
            "{refusal}"
        );
    }
}
