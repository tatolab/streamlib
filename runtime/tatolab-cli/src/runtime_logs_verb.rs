// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs`: a project's log as the machine's runtime reads it from the project's folder —
//! every record its streams and their loads wrote, oldest first, or those of one stream or one
//! stream action. `-f` follows until Ctrl-C, which leaves every stream as it is, and ends with an
//! error naming the runtime's log when the runtime goes away.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListStreamsToolResult, ListStreamsToolResultStream,
};
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, Source};

use crate::TatolabCommandFailure;
use crate::local_api_connection::LocalApiConnection;
use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClientFailure, ONE_SHOT_VERB_TOOL_CALL_TIMEOUT,
    tool_call_failure_worded_as_an_observation_verb_reports_it,
};
use crate::machine_runtime_local_api_socket::local_api_socket_of_the_running_runtime;
use crate::process_signal_handling::{
    an_interrupt_was_delivered_during_the_read, end_the_read_on_interrupt,
};
use crate::project_log_records_from_the_runtime::{
    DESCRIBE_RUNTIME_TOOL_NAME, LOGS_TOOL_NAME, PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL,
    ProjectLogRecordsAsked, project_log_records_page_from, render_project_log_records_page,
};
use crate::runtime_gone_while_following_a_stream::{
    RuntimeLogDirectoryAsDescribed, the_runtime_is_gone,
};
use crate::runtime_log_record_decoding_and_filters::RuntimeLogRecordFilters;
use crate::stream_actions_on_the_runtime::{StreamActionTool, call_one_stream_action_tool};
use crate::stream_argument_resolution_through_the_listing::stream_id_the_stream_argument_names;
use crate::verb_standard_output::standard_output_closed_or_failed;

/// `tatolab logs`' flags.
#[derive(Args, Debug)]
pub(crate) struct RuntimeLogsVerbArguments {
    /// The project whose log is read (default: CWD, no walk-up).
    #[arg(long = "dir", value_name = "DIR")]
    pub(crate) requested_project_directory: Option<PathBuf>,
    /// Only the records of this stream: its id, or a display name exactly one stream the runtime
    /// lists holds (default: every record of the project).
    #[arg(long = "stream", value_name = "STREAM")]
    pub(crate) requested_stream: Option<String>,
    /// Only the records of this stream action, by its action id.
    #[arg(long = "action", value_name = "ID")]
    pub(crate) requested_action: Option<String>,
    /// Follow as new records land (like `tail -F`), by the cursor each page answers.
    #[arg(short = 'f', long = "follow")]
    pub(crate) follow_appended_records: bool,
    /// Only records from this processor id.
    #[arg(long = "processor", value_name = "ID")]
    pub(crate) processor_id: Option<String>,
    /// Only records from this pipeline id.
    #[arg(long = "pipeline", value_name = "ID")]
    pub(crate) pipeline_id: Option<String>,
    /// Only RHI operations (records with rhi_op).
    #[arg(long = "rhi")]
    pub(crate) rhi_operations_only: bool,
    /// Minimum severity to show.
    #[arg(
        long = "level",
        value_name = "LEVEL",
        value_parser = value_parser_of_the_jsonl_spellings_of(&LogLevel::ALL, LogLevel::as_str)
    )]
    pub(crate) minimum_level: Option<LogLevel>,
    /// Only records emitted by this runtime language.
    #[arg(
        long = "source",
        value_name = "SOURCE",
        value_parser = value_parser_of_the_jsonl_spellings_of(&Source::ALL, Source::as_str)
    )]
    pub(crate) source: Option<Source>,
    /// Only intercepted records (captured stdout/stderr/print).
    #[arg(long = "intercepted-only")]
    pub(crate) intercepted_only: bool,
}

impl RuntimeLogsVerbArguments {
    /// The `--processor` … `--intercepted-only` narrowing these flags ask for.
    fn record_filters(&self) -> RuntimeLogRecordFilters {
        RuntimeLogRecordFilters {
            processor_id: self.processor_id.clone(),
            pipeline_id: self.pipeline_id.clone(),
            rhi_operations_only: self.rhi_operations_only,
            minimum_level: self.minimum_level,
            source: self.source,
            intercepted_only: self.intercepted_only,
        }
    }
}

/// A flag's value parser that takes exactly the JSONL record's spellings of `every_value`, listed
/// in that order, and answers the value spelled.
fn value_parser_of_the_jsonl_spellings_of<JsonlSpelledValue>(
    every_value: &'static [JsonlSpelledValue],
    jsonl_spelling_of: fn(&JsonlSpelledValue) -> &'static str,
) -> impl TypedValueParser<Value = JsonlSpelledValue>
where
    JsonlSpelledValue: Copy + Send + Sync + 'static,
{
    PossibleValuesParser::new(every_value.iter().map(jsonl_spelling_of)).try_map(
        move |spelled_value: String| {
            every_value
                .iter()
                .copied()
                .find(|value| jsonl_spelling_of(value) == spelled_value)
                .ok_or_else(|| format!("`{spelled_value}` is not a spelling the record uses"))
        },
    )
}

/// `tatolab logs`: the project's records the flags ask for, read from the runtime.
pub(crate) fn run_runtime_logs_verb(
    logs_arguments: RuntimeLogsVerbArguments,
) -> Result<u8, TatolabCommandFailure> {
    let project_directory =
        crate::stream_actions_on_the_runtime::the_project_directory_a_verb_names(
            &crate::caller_working_directory()?,
            logs_arguments.requested_project_directory.as_deref(),
        )?;
    let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
    let stream_id = match logs_arguments.requested_stream.as_deref() {
        None => None,
        Some(stream_argument) => {
            let list_streams_result: ListStreamsToolResult =
                call_one_stream_action_tool(StreamActionTool::ListStreams, serde_json::Map::new())?;
            let streams_listed_from_the_project: Vec<ListStreamsToolResultStream> =
                list_streams_result
                    .streams
                    .into_iter()
                    .filter(|listed_stream| {
                        listed_stream
                            .location
                            .as_ref()
                            .is_some_and(|location| location.project_directory == project_directory)
                    })
                    .collect();
            Some(stream_id_the_logs_stream_argument_names(
                &streams_listed_from_the_project,
                stream_argument,
            )?)
        }
    };
    route_ctrl_c_to_the_end_of_the_read()?;
    let mut local_api_connection =
        LocalApiConnection::open(&local_api_socket_path, ONE_SHOT_VERB_TOOL_CALL_TIMEOUT)?;
    let runtime_log_directory = if logs_arguments.follow_appended_records {
        RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(
            local_api_connection.call_tool(DESCRIBE_RUNTIME_TOOL_NAME, serde_json::Map::new()),
        )
    } else {
        RuntimeLogDirectoryAsDescribed::NotDescribed
    };
    let project_log_records_asked = ProjectLogRecordsAsked {
        project_directory: &project_directory,
        stream: stream_id.as_deref(),
        action: logs_arguments.requested_action.as_deref(),
    };
    let standard_output = io::stdout();
    let mut buffered_standard_output = io::BufWriter::new(standard_output.lock());
    print_project_log_records_read_from_the_runtime(
        &mut |after| {
            local_api_connection.call_tool(
                LOGS_TOOL_NAME,
                project_log_records_asked.logs_tool_arguments_after(after),
            )
        },
        &ProjectLogReadRequest {
            follow_appended_records: logs_arguments.follow_appended_records,
            record_filters: logs_arguments.record_filters(),
        },
        &mut buffered_standard_output,
        &mut io::stderr(),
        &HowAFollowedReadOfTheRuntimeGoesOn {
            read_interrupted: &an_interrupt_was_delivered_during_the_read,
            follow_poll_interval: PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL,
            runtime_log_directory: &runtime_log_directory,
        },
    )
}

/// The stream id `stream_argument` names among `listed_streams`, the project's — a listed id, else a display
/// name only one of them holds — or, naming none of them, the argument itself: the id of a stream
/// whose records outlive its record. A display name several hold is refused listing each.
fn stream_id_the_logs_stream_argument_names(
    listed_streams: &[ListStreamsToolResultStream],
    stream_argument: &str,
) -> Result<String, TatolabCommandFailure> {
    let names_a_listed_stream = listed_streams.iter().any(|listed_stream| {
        listed_stream.stream == stream_argument || listed_stream.display_name == stream_argument
    });
    if !stream_argument.is_empty() && !names_a_listed_stream {
        return Ok(stream_argument.to_owned());
    }
    stream_id_the_stream_argument_names(listed_streams, Some(stream_argument), || {
        Err(TatolabCommandFailure::refused(
            "a named stream reads no working directory".to_owned(),
        ))
    })
}

/// What a read of a project's records keeps, and whether it follows past the newest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectLogReadRequest {
    /// Keep reading as records land.
    pub(crate) follow_appended_records: bool,
    /// The records a read keeps.
    pub(crate) record_filters: RuntimeLogRecordFilters,
}

/// How a read of a project's records from the runtime goes on once it reached the newest: when it
/// ends, how long it waits for more, and where the runtime's log is when it goes away.
pub(crate) struct HowAFollowedReadOfTheRuntimeGoesOn<'read> {
    /// Answers true once the read is to end.
    pub(crate) read_interrupted: &'read dyn Fn() -> bool,
    /// How long a follow waits after a page that brought nothing.
    pub(crate) follow_poll_interval: Duration,
    /// Where the runtime said its own log is, named when it goes away mid-follow.
    pub(crate) runtime_log_directory: &'read RuntimeLogDirectoryAsDescribed,
}

fn route_ctrl_c_to_the_end_of_the_read() -> Result<(), TatolabCommandFailure> {
    end_the_read_on_interrupt().map_err(|interrupt_routing_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot route Ctrl-C to the end of the read: {interrupt_routing_failure}"
        ))
    })
}

/// A project's records, each page read by `read_records_page_after` from a cursor, rendered
/// through the request's filters onto `standard_output` from the oldest record the project's log
/// holds: to the newest when not following, and on as `how_it_goes_on` says when following. A
/// runtime gone mid-follow ends the read with an error naming its log.
pub(crate) fn print_project_log_records_read_from_the_runtime(
    read_records_page_after: &mut dyn FnMut(
        Option<&str>,
    ) -> Result<String, LocalApiMcpToolClientFailure>,
    project_log_read_request: &ProjectLogReadRequest,
    standard_output: &mut dyn Write,
    standard_error: &mut dyn Write,
    how_it_goes_on: &HowAFollowedReadOfTheRuntimeGoesOn<'_>,
) -> Result<u8, TatolabCommandFailure> {
    let mut after: Option<String> = None;
    while !(how_it_goes_on.read_interrupted)() {
        let logs_tool_result_text =
            read_records_page_after(after.as_deref()).map_err(|logs_failure| {
                let _ = standard_output.flush();
                if project_log_read_request.follow_appended_records
                    && the_runtime_is_gone(&logs_failure)
                {
                    return TatolabCommandFailure::refused(
                        how_it_goes_on
                            .runtime_log_directory
                            .the_runtime_went_away_message(),
                    );
                }
                tool_call_failure_worded_as_an_observation_verb_reports_it(
                    LOGS_TOOL_NAME,
                    logs_failure,
                )
                .into()
            })?;
        let project_log_records_page = project_log_records_page_from(&logs_tool_result_text)
            .map_err(TatolabCommandFailure::refused)?;
        let rendered_records = render_project_log_records_page(
            &project_log_records_page,
            &project_log_read_request.record_filters,
            false,
            standard_error,
        );
        let page_brought_nothing = project_log_records_page.records.is_empty();
        after = Some(project_log_records_page.next_after);
        let written = standard_output
            .write_all(rendered_records.as_bytes())
            .and_then(|()| {
                if page_brought_nothing {
                    standard_output.flush()
                } else {
                    Ok(())
                }
            });
        if let Err(write_failure) = written {
            return standard_output_closed_or_failed(write_failure);
        }
        if page_brought_nothing {
            if !project_log_read_request.follow_appended_records {
                return Ok(0);
            }
            std::thread::sleep(how_it_goes_on.follow_poll_interval);
        }
    }
    match standard_output.flush() {
        Ok(()) => Ok(0),
        Err(write_failure) => standard_output_closed_or_failed(write_failure),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use clap::Parser;

    use super::*;
    use crate::runtime_log_line_fixtures::a_log_record;
    use crate::stream_actions_on_the_runtime::the_project_directory_a_verb_names;
    use std::path::Path;

    #[derive(Parser)]
    struct LogsVerbCommandLine {
        #[command(flatten)]
        logs_arguments: RuntimeLogsVerbArguments,
    }

    fn logs_arguments_parsed_from(logs_flags: &[&str]) -> RuntimeLogsVerbArguments {
        LogsVerbCommandLine::try_parse_from([&["logs"], logs_flags].concat())
            .unwrap()
            .logs_arguments
    }

    /// The pages `read_records_page_after` serves: at most `page_size` of `records` after each
    /// cursor `1:<n>`, every cursor asked for recorded.
    fn paging_through(
        records: Vec<serde_json::Value>,
        page_size: usize,
        afters_asked_for: &RefCell<Vec<Option<String>>>,
    ) -> impl FnMut(Option<&str>) -> Result<String, LocalApiMcpToolClientFailure> + '_ {
        move |after| {
            afters_asked_for.borrow_mut().push(after.map(str::to_owned));
            let lines_before: usize = after
                .and_then(|cursor| cursor.strip_prefix("1:"))
                .map_or(0, |lines_before| lines_before.parse().unwrap());
            let page_records: Vec<serde_json::Value> = records
                .iter()
                .skip(lines_before)
                .take(page_size)
                .cloned()
                .collect();
            Ok(serde_json::json!({
                "project_directory": "/srv/desk",
                "next_after": format!("1:{}", lines_before + page_records.len()),
                "records": page_records,
                "records_no_longer_held": false,
            })
            .to_string())
        }
    }

    fn not_following() -> HowAFollowedReadOfTheRuntimeGoesOn<'static> {
        HowAFollowedReadOfTheRuntimeGoesOn {
            read_interrupted: &|| false,
            follow_poll_interval: Duration::ZERO,
            runtime_log_directory: &RuntimeLogDirectoryAsDescribed::NotDescribed,
        }
    }

    #[test]
    fn a_read_pages_by_cursor_to_the_newest_record_and_ends() {
        let afters_asked_for = RefCell::new(Vec::new());
        let mut standard_output = Vec::new();

        let printed = print_project_log_records_read_from_the_runtime(
            &mut paging_through(
                ["one", "two", "three"]
                    .map(|message| a_log_record(serde_json::json!({"message": message})))
                    .to_vec(),
                2,
                &afters_asked_for,
            ),
            &ProjectLogReadRequest::default(),
            &mut standard_output,
            &mut Vec::new(),
            &not_following(),
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(
            String::from_utf8(standard_output).unwrap(),
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — one\n\
             21:04:27.573 [ INFO] [Rabc/rust] tatolabd — two\n\
             21:04:27.573 [ INFO] [Rabc/rust] tatolabd — three\n"
        );
        assert_eq!(
            *afters_asked_for.borrow(),
            [None, Some("1:2".to_owned()), Some("1:3".to_owned())]
        );
    }

    #[test]
    fn a_followed_read_keeps_asking_after_the_newest_until_interrupted() {
        let afters_asked_for = RefCell::new(Vec::new());
        let read_interrupted = || afters_asked_for.borrow().len() >= 4;

        let printed = print_project_log_records_read_from_the_runtime(
            &mut paging_through(
                vec![a_log_record(serde_json::json!({}))],
                5,
                &afters_asked_for,
            ),
            &ProjectLogReadRequest {
                follow_appended_records: true,
                ..Default::default()
            },
            &mut Vec::new(),
            &mut Vec::new(),
            &HowAFollowedReadOfTheRuntimeGoesOn {
                read_interrupted: &read_interrupted,
                ..not_following()
            },
        );

        assert_eq!(printed.unwrap(), 0);
        let read_on_from = Some("1:1".to_owned());
        assert_eq!(
            *afters_asked_for.borrow(),
            [
                None,
                read_on_from.clone(),
                read_on_from.clone(),
                read_on_from
            ]
        );
    }

    #[test]
    fn a_read_the_runtime_refuses_is_refused_in_its_words() {
        let printed = print_project_log_records_read_from_the_runtime(
            &mut |_after| {
                Err(LocalApiMcpToolClientFailure::tool_call_failed(
                    "logs failed: `seven` is not a log cursor".to_owned(),
                ))
            },
            &ProjectLogReadRequest::default(),
            &mut Vec::new(),
            &mut Vec::new(),
            &not_following(),
        );

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(printed),
            "logs failed: `seven` is not a log cursor"
        );
    }

    #[test]
    fn level_and_source_take_the_names_the_jsonl_record_spells() {
        let logs_arguments = logs_arguments_parsed_from(&["--level", "trace", "--source", "rust"]);

        assert_eq!(
            logs_arguments.record_filters(),
            RuntimeLogRecordFilters {
                minimum_level: Some(LogLevel::Trace),
                source: Some(Source::Rust),
                ..Default::default()
            }
        );
        for unknown_value_flags in [["--level", "fatal"], ["--source", "go"]] {
            assert!(
                LogsVerbCommandLine::try_parse_from([&["logs"][..], &unknown_value_flags].concat())
                    .is_err(),
                "{unknown_value_flags:?}"
            );
        }
    }

    #[test]
    fn logs_takes_no_positional_file_name_or_list() {
        for removed in [&["Rabc-camera"][..], &["--list"]] {
            assert!(
                LogsVerbCommandLine::try_parse_from([&["logs"][..], removed].concat()).is_err(),
                "{removed:?}"
            );
        }
    }

    #[test]
    fn the_project_is_the_canonical_dir_else_the_working_directory() {
        let working_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(working_directory.path().join("desk")).unwrap();
        let canonical_working_directory = working_directory.path().canonicalize().unwrap();

        assert_eq!(
            the_project_directory_a_verb_names(working_directory.path(), None).unwrap(),
            canonical_working_directory
        );
        assert_eq!(
            the_project_directory_a_verb_names(working_directory.path(), Some(Path::new("desk")))
                .unwrap(),
            canonical_working_directory.join("desk")
        );
        assert_eq!(
            TatolabCommandFailure::refusal_message_of(the_project_directory_a_verb_names(
                working_directory.path(),
                Some(Path::new("gone"))
            )),
            "--dir gone is not a directory"
        );
    }

    fn a_listed_stream(stream: &str, display_name: &str) -> ListStreamsToolResultStream {
        serde_json::from_value(serde_json::json!({
            "stream": stream, "display_name": display_name, "state": "running",
            "location": null, "node_count": 1, "failed_because": null,
        }))
        .unwrap()
    }

    #[test]
    fn a_stream_argument_resolves_a_display_name_and_passes_an_unlisted_id_through() {
        let listed_streams = [
            a_listed_stream("desk-camera-0a1b", "camera"),
            a_listed_stream("desk-preview-2c3d", "preview"),
            a_listed_stream("hall-preview-4e5f", "preview"),
        ];

        assert_eq!(
            stream_id_the_logs_stream_argument_names(&listed_streams, "camera").unwrap(),
            "desk-camera-0a1b"
        );
        assert_eq!(
            stream_id_the_logs_stream_argument_names(&listed_streams, "desk-preview-2c3d").unwrap(),
            "desk-preview-2c3d"
        );
        assert_eq!(
            stream_id_the_logs_stream_argument_names(&listed_streams, "removed-9a8b").unwrap(),
            "removed-9a8b"
        );
        let ambiguous = TatolabCommandFailure::refusal_message_of(
            stream_id_the_logs_stream_argument_names(&listed_streams, "preview"),
        );
        assert!(
            ambiguous.contains("desk-preview-2c3d") && ambiguous.contains("hall-preview-4e5f"),
            "{ambiguous}"
        );
        assert!(stream_id_the_logs_stream_argument_names(&listed_streams, "").is_err());
    }
}
