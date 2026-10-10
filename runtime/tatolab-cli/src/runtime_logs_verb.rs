// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs`: with RUNTIME_ID-STREAM, one loaded stream's on-disk JSONL log rendered as the
//! runtime mirrored it; with `--list`, the stream logs on disk; with `--stream`, a loaded stream's
//! records as the machine's runtime holds them, read by sequence number.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Datelike};
use clap::Args;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, Source};
use streamlib_runtime_client_contract::runtime_log_file_paths::{
    RuntimeLogInstanceOnDisk, log_dir, newest_runtime_log_instance_in_directory,
    runtime_log_instances_in_directory,
};

use crate::TatolabCommandFailure;
use crate::local_api_connection::LocalApiConnection;
use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClientFailure, OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    tool_call_failure_worded_as_an_observation_verb_reports_it,
};
use crate::machine_runtime_local_api_socket::local_api_socket_of_the_running_runtime;
use crate::process_signal_handling::{
    an_interrupt_was_delivered_during_the_read, end_the_read_on_interrupt,
};
use crate::runtime_log_files_reader::{
    RUNTIME_LOG_FOLLOW_POLL_INTERVAL, RuntimeLogReadFailure, RuntimeLogReadStep,
    RuntimeLogRecordFilters, RuntimeLogRecordsReader,
};
use crate::stream_log_records_from_the_runtime::{
    LOGS_TOOL_NAME, STREAM_LOG_RECORDS_FOLLOW_POLL_INTERVAL, logs_tool_arguments_after,
    render_stream_log_records_page, stream_log_records_page_from,
};
use crate::verb_standard_output::standard_output_closed_or_failed;

/// The width `--list` pads its RUNTIME_ID-STREAM and STARTED_AT columns to.
const RUNTIME_LOG_LISTING_COLUMN_WIDTH: usize = 24;

/// The latest year an ISO-8601 STARTED_AT is written for; a later start is shown as its number.
const LATEST_STARTED_AT_YEAR_RENDERED_AS_A_DATE: i32 = 9999;

/// `tatolab logs`' flags.
#[derive(Args, Debug)]
pub(crate) struct RuntimeLogsVerbArguments {
    /// The stream log to read: `<runtime_id>-<stream>`, as --list names it. Omit with --list or
    /// --stream.
    #[arg(value_name = "RUNTIME_ID-STREAM")]
    pub(crate) stream_log_instance_name: Option<String>,
    /// Enumerate the stream logs on disk instead of reading one.
    #[arg(long = "list")]
    pub(crate) list_stream_logs_on_disk: bool,
    /// Read this loaded stream's records from the running runtime instead of a file on disk.
    #[arg(long = "stream", value_name = "STREAM")]
    pub(crate) requested_stream: Option<String>,
    /// Follow as new records land (like `tail -F`): the log file across rotations, or with
    /// --stream the runtime's records by sequence number.
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

/// What `logs` was asked to do on disk, once `--stream` is ruled out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OnDiskRuntimeLogRequest {
    /// The stream log, `<runtime_id>-<stream>`, whose newest instance is read.
    pub(crate) stream_log_instance_name: Option<String>,
    /// List the stream logs on disk rather than read one.
    pub(crate) list_stream_logs_on_disk: bool,
    /// Keep reading as records land, across rotations and restarts.
    pub(crate) follow_appended_records: bool,
    /// The records a read keeps.
    pub(crate) record_filters: RuntimeLogRecordFilters,
}

impl OnDiskRuntimeLogRequest {
    /// The on-disk flags given, in the order `logs --help` lists them; a flag whose value is
    /// empty counts as not given.
    fn on_disk_flags_given(&self) -> Vec<&'static str> {
        let record_filters = &self.record_filters;
        [
            (
                "RUNTIME_ID-STREAM",
                is_given(&self.stream_log_instance_name),
            ),
            ("--list", self.list_stream_logs_on_disk),
            ("--follow", self.follow_appended_records),
            ("--processor", is_given(&record_filters.processor_id)),
            ("--pipeline", is_given(&record_filters.pipeline_id)),
            ("--rhi", record_filters.rhi_operations_only),
            ("--level", record_filters.minimum_level.is_some()),
            ("--source", record_filters.source.is_some()),
            ("--intercepted-only", record_filters.intercepted_only),
        ]
        .into_iter()
        .filter_map(|(flag_name, given)| given.then_some(flag_name))
        .collect()
    }

    /// [`Self::on_disk_flags_given`] less `--list`: what a listing would leave unread.
    fn flags_given_besides_list(&self) -> Vec<&'static str> {
        self.on_disk_flags_given()
            .into_iter()
            .filter(|&flag_name| flag_name != "--list")
            .collect()
    }

    /// The flags given that name a file on disk, which a read from the runtime has no use for.
    fn on_disk_only_flags_given(&self) -> Vec<&'static str> {
        self.on_disk_flags_given()
            .into_iter()
            .filter(|&flag_name| matches!(flag_name, "RUNTIME_ID-STREAM" | "--list"))
            .collect()
    }
}

fn is_given(flag_value: &Option<String>) -> bool {
    flag_value
        .as_deref()
        .is_some_and(|flag_value| !flag_value.is_empty())
}

impl From<&RuntimeLogsVerbArguments> for OnDiskRuntimeLogRequest {
    fn from(logs_arguments: &RuntimeLogsVerbArguments) -> Self {
        Self {
            stream_log_instance_name: logs_arguments.stream_log_instance_name.clone(),
            list_stream_logs_on_disk: logs_arguments.list_stream_logs_on_disk,
            follow_appended_records: logs_arguments.follow_appended_records,
            record_filters: RuntimeLogRecordFilters {
                processor_id: logs_arguments.processor_id.clone(),
                pipeline_id: logs_arguments.pipeline_id.clone(),
                rhi_operations_only: logs_arguments.rhi_operations_only,
                minimum_level: logs_arguments.minimum_level,
                source: logs_arguments.source,
                intercepted_only: logs_arguments.intercepted_only,
            },
        }
    }
}

/// `tatolab logs`: `--stream` reads a loaded stream's records from the runtime, anything else
/// reads the disk.
///
/// A file's name and `--list` have no meaning against the runtime, so asking for both is refused
/// rather than silently ignoring the flag.
pub(crate) fn run_runtime_logs_verb(
    logs_arguments: RuntimeLogsVerbArguments,
) -> Result<u8, TatolabCommandFailure> {
    let on_disk_request = OnDiskRuntimeLogRequest::from(&logs_arguments);
    if let Some(requested_stream) = logs_arguments
        .requested_stream
        .as_deref()
        .filter(|requested_stream| !requested_stream.is_empty())
    {
        let on_disk_only_flags_given = on_disk_request.on_disk_only_flags_given();
        if !on_disk_only_flags_given.is_empty() {
            return Err(TatolabCommandFailure::refused(format!(
                "`--stream` reads a loaded stream's records from the runtime, which takes no {}. \
                 Drop `--stream` to read an on-disk log file instead.",
                on_disk_only_flags_given.join(", ")
            )));
        }
        let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
        route_ctrl_c_to_the_end_of_the_read()?;
        let mut local_api_connection =
            LocalApiConnection::open(&local_api_socket_path, OBSERVATION_VERB_TOOL_CALL_TIMEOUT)?;
        let standard_output = io::stdout();
        let mut buffered_standard_output = io::BufWriter::new(standard_output.lock());
        return print_stream_log_records_read_from_the_runtime(
            requested_stream,
            &mut |after| {
                local_api_connection.call_tool(
                    LOGS_TOOL_NAME,
                    logs_tool_arguments_after(requested_stream, after),
                )
            },
            &on_disk_request,
            &mut buffered_standard_output,
            &mut io::stderr(),
            &an_interrupt_was_delivered_during_the_read,
            STREAM_LOG_RECORDS_FOLLOW_POLL_INTERVAL,
        );
    }
    route_ctrl_c_to_the_end_of_the_read()?;
    let standard_output = io::stdout();
    let mut buffered_standard_output = io::BufWriter::new(standard_output.lock());
    print_stream_log_files_on_disk(
        &log_dir(),
        &on_disk_request,
        &mut buffered_standard_output,
        &mut io::stderr(),
        &an_interrupt_was_delivered_during_the_read,
        RUNTIME_LOG_FOLLOW_POLL_INTERVAL,
    )
}

fn route_ctrl_c_to_the_end_of_the_read() -> Result<(), TatolabCommandFailure> {
    end_the_read_on_interrupt().map_err(|interrupt_routing_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot route Ctrl-C to the end of the read: {interrupt_routing_failure}"
        ))
    })
}

/// `stream`'s records, each page read by `read_records_page_after` from a sequence number,
/// rendered through the request's filters onto `standard_output` from the first record the
/// runtime holds: to the newest when not following, and on until `read_interrupted` answers true
/// when following, waiting `follow_poll_interval` after a page that brought nothing.
pub(crate) fn print_stream_log_records_read_from_the_runtime(
    stream: &str,
    read_records_page_after: &mut dyn FnMut(u64) -> Result<String, LocalApiMcpToolClientFailure>,
    stream_log_request: &OnDiskRuntimeLogRequest,
    standard_output: &mut dyn Write,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Result<u8, TatolabCommandFailure> {
    let mut after = 0;
    while !read_interrupted() {
        let logs_tool_result_text = read_records_page_after(after).map_err(|logs_failure| {
            let _ = standard_output.flush();
            tool_call_failure_worded_as_an_observation_verb_reports_it(LOGS_TOOL_NAME, logs_failure)
        })?;
        let stream_log_records_page = stream_log_records_page_from(&logs_tool_result_text)
            .map_err(TatolabCommandFailure::refused)?;
        let rendered_records = render_stream_log_records_page(
            stream,
            &stream_log_records_page,
            &stream_log_request.record_filters,
            standard_error,
        );
        let page_brought_nothing = stream_log_records_page.next_after == after;
        after = stream_log_records_page.next_after;
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
            if !stream_log_request.follow_appended_records {
                return Ok(0);
            }
            std::thread::sleep(follow_poll_interval);
        }
    }
    match standard_output.flush() {
        Ok(()) => Ok(0),
        Err(write_failure) => standard_output_closed_or_failed(write_failure),
    }
}

/// The on-disk side of `logs` against the log files in `log_directory`: `--list`, or one
/// stream log's records — followed until `read_interrupted` answers true when following.
pub(crate) fn print_stream_log_files_on_disk(
    log_directory: &Path,
    on_disk_request: &OnDiskRuntimeLogRequest,
    standard_output: &mut dyn Write,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Result<u8, TatolabCommandFailure> {
    if on_disk_request.list_stream_logs_on_disk {
        let flags_ignored_beside_list = on_disk_request.flags_given_besides_list();
        if !flags_ignored_beside_list.is_empty() {
            return Err(TatolabCommandFailure::refused(format!(
                "`--list` enumerates the stream logs on disk and reads none of them, so it takes \
                 no {}.",
                flags_ignored_beside_list.join(", ")
            )));
        }
        let runtime_log_listing = render_runtime_log_instance_listing(
            log_directory,
            runtime_log_instances_in_directory(log_directory).map_err(|listing_failure| {
                runtime_log_directory_unreadable(log_directory, listing_failure)
            })?,
        );
        return match standard_output
            .write_all(runtime_log_listing.as_bytes())
            .and_then(|()| standard_output.flush())
        {
            Ok(()) => Ok(0),
            Err(write_failure) => standard_output_closed_or_failed(write_failure),
        };
    }
    let Some(stream_log_instance_name) = on_disk_request.stream_log_instance_name.as_deref() else {
        return Err(TatolabCommandFailure::refused(
            "missing RUNTIME_ID-STREAM.\n`tatolab logs --list` enumerates the stream logs on \
             disk, and `--stream` reads a loaded stream's records from the runtime instead."
                .to_owned(),
        ));
    };
    let newest_instance =
        newest_runtime_log_instance_in_directory(log_directory, stream_log_instance_name).map_err(
            |listing_failure| runtime_log_directory_unreadable(log_directory, listing_failure),
        )?;
    let instance_to_read = match newest_instance {
        Some(instance_to_read) => instance_to_read,
        None if !on_disk_request.follow_appended_records => {
            return Err(TatolabCommandFailure::refused(format!(
                "no stream log `{stream_log_instance_name}` in {}.\nA runtime logs per loaded stream, as \
                 `<runtime_id>-<stream>`; `tatolab logs --list` names each one.",
                log_directory.display()
            )));
        }
        None => match wait_for_the_first_segment_of_the_stream_log(
            log_directory,
            stream_log_instance_name,
            standard_error,
            read_interrupted,
            follow_poll_interval,
        )? {
            Some(instance_to_read) => instance_to_read,
            None => return Ok(0),
        },
    };
    let mut runtime_log_records_reader = RuntimeLogRecordsReader::reading(
        log_directory,
        instance_to_read,
        on_disk_request.record_filters.clone(),
        on_disk_request.follow_appended_records,
    );
    match print_rendered_records_until_the_read_ends(
        &mut runtime_log_records_reader,
        standard_output,
        standard_error,
        read_interrupted,
        follow_poll_interval,
    ) {
        Ok(()) => Ok(0),
        Err(RuntimeLogRecordsPrintFailure::StandardOutputNotWritable(write_failure)) => {
            standard_output_closed_or_failed(write_failure)
        }
        Err(RuntimeLogRecordsPrintFailure::RuntimeLogNotReadable(runtime_log_read_failure)) => {
            let _ = standard_output.flush();
            Err(TatolabCommandFailure::refused(
                runtime_log_read_failure.to_string(),
            ))
        }
    }
}

/// Why printing a runtime's rendered records stopped before the read ended.
#[derive(Debug, thiserror::Error)]
enum RuntimeLogRecordsPrintFailure {
    /// Standard output refused a write or a flush.
    #[error(transparent)]
    StandardOutputNotWritable(#[from] io::Error),
    /// A segment or the log directory could not be read.
    #[error(transparent)]
    RuntimeLogNotReadable(#[from] RuntimeLogReadFailure),
}

/// Print `runtime_log_records_reader`'s rendered records to `standard_output` until the read
/// finishes or `read_interrupted` answers true; at each live edge, flush what was printed, then
/// wait `follow_poll_interval` for more.
fn print_rendered_records_until_the_read_ends(
    runtime_log_records_reader: &mut RuntimeLogRecordsReader,
    standard_output: &mut dyn Write,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Result<(), RuntimeLogRecordsPrintFailure> {
    while !read_interrupted() {
        match runtime_log_records_reader.next_step(standard_error)? {
            RuntimeLogReadStep::RenderedRecord(rendered_record) => {
                standard_output.write_all(rendered_record.as_bytes())?;
            }
            RuntimeLogReadStep::LiveEdgeReached => {
                standard_output.flush()?;
                std::thread::sleep(follow_poll_interval);
            }
            RuntimeLogReadStep::Finished => break,
        }
    }
    standard_output.flush()?;
    Ok(())
}

/// Wait for the first segment of the stream log `stream_log_instance_name`, for `--follow` before
/// the stream loads; `None` when the wait was interrupted.
///
/// Following a stream log whose stream is about to load is the point of `--follow`; failing
/// because the file does not exist yet would refuse the one case the flag is for.
fn wait_for_the_first_segment_of_the_stream_log(
    log_directory: &Path,
    stream_log_instance_name: &str,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Result<Option<RuntimeLogInstanceOnDisk>, TatolabCommandFailure> {
    let _ = writeln!(
        standard_error,
        "note: no stream log '{stream_log_instance_name}' yet, waiting in --follow mode..."
    );
    loop {
        if let Some(first_instance) =
            newest_runtime_log_instance_in_directory(log_directory, stream_log_instance_name)
                .map_err(|listing_failure| {
                    runtime_log_directory_unreadable(log_directory, listing_failure)
                })?
        {
            return Ok(Some(first_instance));
        }
        if read_interrupted() {
            return Ok(None);
        }
        std::thread::sleep(follow_poll_interval);
    }
}

/// The refusal for a log directory that exists and cannot be listed.
fn runtime_log_directory_unreadable(
    log_directory: &Path,
    listing_failure: std::io::Error,
) -> TatolabCommandFailure {
    TatolabCommandFailure::refused(
        RuntimeLogReadFailure::of_log_directory(log_directory, listing_failure).to_string(),
    )
}

/// What `--list` prints for `runtime_log_instances` found in `log_directory`: newest started
/// first, or one line naming the directory when it holds none.
pub(crate) fn render_runtime_log_instance_listing(
    log_directory: &Path,
    mut runtime_log_instances: Vec<RuntimeLogInstanceOnDisk>,
) -> String {
    if runtime_log_instances.is_empty() {
        return format!("(no stream logs in {})\n", log_directory.display());
    }
    runtime_log_instances
        .sort_by(|earlier_listed, later_listed| later_listed.compare_started_at(earlier_listed));
    let mut runtime_log_listing = format!(
        "{:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  SIZE\n",
        "RUNTIME_ID-STREAM", "STARTED_AT"
    );
    for runtime_log_instance in &runtime_log_instances {
        let _ = writeln!(
            runtime_log_listing,
            "{:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {}",
            runtime_log_instance.runtime_id,
            format_started_at(&runtime_log_instance.started_at_millis_digits),
            format_size(runtime_log_instance.total_segment_bytes)
        );
    }
    runtime_log_listing
}

/// A start in epoch milliseconds as an ISO-8601 UTC stamp, or the number itself past year 9999.
///
/// The name a start is parsed from is unbounded, so one stray file in the directory reaches this;
/// failing here would hide every healthy runtime in the same listing.
pub(crate) fn format_started_at(started_at_millis_digits: &str) -> String {
    started_at_millis_digits
        .parse::<i64>()
        .ok()
        .and_then(|started_at_millis| {
            DateTime::from_timestamp(started_at_millis.div_euclid(1000), 0)
        })
        .filter(|started_at| started_at.year() <= LATEST_STARTED_AT_YEAR_RENDERED_AS_A_DATE)
        .map(|started_at| started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| {
            let significant_digits = started_at_millis_digits.trim_start_matches('0');
            if significant_digits.is_empty() {
                "0".to_owned()
            } else {
                significant_digits.to_owned()
            }
        })
}

/// A byte count in binary units, one decimal from KiB up.
pub(crate) fn format_size(size_bytes: u64) -> String {
    for (unit_name, unit_bytes) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if size_bytes >= unit_bytes {
            return format!("{:.1} {unit_name}", size_bytes as f64 / unit_bytes as f64);
        }
    }
    format!("{size_bytes} B")
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use clap::Parser;

    use super::*;
    use crate::runtime_log_line_fixtures::{a_log_line_with_message, a_log_record};

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

    #[test]
    fn a_stream_with_a_file_name_or_list_is_refused_naming_each() {
        let refused = run_runtime_logs_verb(logs_arguments_parsed_from(&[
            "--level", "warn", "Rabc", "--stream", "camera", "--list", "-f",
        ]));

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(refused),
            "`--stream` reads a loaded stream's records from the runtime, which takes no \
             RUNTIME_ID-STREAM, --list. Drop `--stream` to read an on-disk log file instead."
        );
    }

    /// The pages `read_records_page_after` serves: `records` numbered from 1, at most
    /// `page_size` after each `after`, every `after` asked for recorded.
    fn paging_through(
        records: Vec<serde_json::Value>,
        page_size: usize,
        afters_asked_for: &RefCell<Vec<u64>>,
    ) -> impl FnMut(u64) -> Result<String, LocalApiMcpToolClientFailure> + '_ {
        move |after| {
            afters_asked_for.borrow_mut().push(after);
            let page_records: Vec<serde_json::Value> = records
                .iter()
                .enumerate()
                .skip(after as usize)
                .take(page_size)
                .map(|(record_index, record)| {
                    serde_json::json!({"sequence": record_index + 1, "record": record})
                })
                .collect();
            Ok(serde_json::json!({
                "stream": "camera",
                "next_after": after + page_records.len() as u64,
                "records": page_records,
                "records_no_longer_held": 0,
            })
            .to_string())
        }
    }

    #[test]
    fn a_stream_read_pages_by_sequence_number_to_the_newest_record_and_ends() {
        let afters_asked_for = RefCell::new(Vec::new());
        let mut standard_output = Vec::new();

        let printed = print_stream_log_records_read_from_the_runtime(
            "camera",
            &mut paging_through(
                ["one", "two", "three"]
                    .map(|message| a_log_record(serde_json::json!({"message": message})))
                    .to_vec(),
                2,
                &afters_asked_for,
            ),
            &OnDiskRuntimeLogRequest::default(),
            &mut standard_output,
            &mut Vec::new(),
            &|| false,
            Duration::ZERO,
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(
            String::from_utf8(standard_output).unwrap(),
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — one\n\
             21:04:27.573 [ INFO] [Rabc/rust] tatolabd — two\n\
             21:04:27.573 [ INFO] [Rabc/rust] tatolabd — three\n"
        );
        assert_eq!(*afters_asked_for.borrow(), [0, 2, 3]);
    }

    #[test]
    fn a_followed_stream_read_keeps_asking_after_the_newest_until_interrupted() {
        let afters_asked_for = RefCell::new(Vec::new());
        let read_interrupted = || afters_asked_for.borrow().len() >= 4;

        let printed = print_stream_log_records_read_from_the_runtime(
            "camera",
            &mut paging_through(
                vec![a_log_record(serde_json::json!({}))],
                5,
                &afters_asked_for,
            ),
            &OnDiskRuntimeLogRequest {
                follow_appended_records: true,
                ..Default::default()
            },
            &mut Vec::new(),
            &mut Vec::new(),
            &read_interrupted,
            Duration::ZERO,
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(*afters_asked_for.borrow(), [0, 1, 1, 1]);
    }

    #[test]
    fn a_stream_the_runtime_refuses_is_refused_in_its_words() {
        let printed = print_stream_log_records_read_from_the_runtime(
            "gone",
            &mut |_after| {
                Err(LocalApiMcpToolClientFailure::tool_call_failed(
                    "logs failed: no stream `gone` is loaded; loaded: camera".to_owned(),
                ))
            },
            &OnDiskRuntimeLogRequest::default(),
            &mut Vec::new(),
            &mut Vec::new(),
            &|| false,
            Duration::ZERO,
        );

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(printed),
            "logs failed: no stream `gone` is loaded; loaded: camera"
        );
    }

    #[test]
    fn level_and_source_take_the_names_the_jsonl_record_spells() {
        let logs_arguments =
            logs_arguments_parsed_from(&["Rabc", "--level", "trace", "--source", "rust"]);

        assert_eq!(
            OnDiskRuntimeLogRequest::from(&logs_arguments).record_filters,
            RuntimeLogRecordFilters {
                minimum_level: Some(LogLevel::Trace),
                source: Some(Source::Rust),
                ..Default::default()
            }
        );
        for unknown_value_flags in [["--level", "fatal"], ["--source", "go"]] {
            assert!(
                LogsVerbCommandLine::try_parse_from(
                    [&["logs", "Rabc"][..], &unknown_value_flags].concat()
                )
                .is_err(),
                "{unknown_value_flags:?}"
            );
        }
    }

    fn runtime_log_instance(
        log_directory: &Path,
        runtime_id: &str,
        started_at_millis_digits: &str,
        total_segment_bytes: u64,
    ) -> RuntimeLogInstanceOnDisk {
        RuntimeLogInstanceOnDisk {
            runtime_id: runtime_id.to_owned(),
            started_at_millis_digits: started_at_millis_digits.to_owned(),
            active_segment_path: log_directory.join(
                streamlib_runtime_client_contract::runtime_log_file_paths::active_runtime_log_segment_file_name(
                    runtime_id,
                    started_at_millis_digits,
                ),
            ),
            total_segment_bytes,
        }
    }

    /// `print_stream_log_files_on_disk` against `log_directory`, never interrupted: stdout, stderr and
    /// the outcome.
    fn print_stream_log_files_on_disk_capturing_output(
        log_directory: &Path,
        on_disk_request: &OnDiskRuntimeLogRequest,
    ) -> (String, String, Result<u8, TatolabCommandFailure>) {
        let mut standard_output = Vec::new();
        let mut standard_error = Vec::new();
        let printed = print_stream_log_files_on_disk(
            log_directory,
            on_disk_request,
            &mut standard_output,
            &mut standard_error,
            &|| false,
            Duration::ZERO,
        );
        (
            String::from_utf8(standard_output).unwrap(),
            String::from_utf8(standard_error).unwrap(),
            printed,
        )
    }

    #[test]
    fn a_started_at_stamp_reads_as_a_date_not_epoch_millis() {
        assert_eq!(format_started_at("1786136667573"), "2026-08-07T21:04:27Z");
        assert_eq!(format_started_at("0"), "1970-01-01T00:00:00Z");
        assert_eq!(format_started_at("253402300799999"), "9999-12-31T23:59:59Z");
    }

    /// The name is parsed unbounded, so one stray file in the log directory reaches this.
    /// Failing here would hide every healthy runtime in the same listing.
    #[test]
    fn an_out_of_range_stamp_degrades_instead_of_taking_the_listing_down() {
        assert_eq!(
            format_started_at("99999999999999999999"),
            "99999999999999999999"
        );
        assert_eq!(format_started_at("253402300800000"), "253402300800000");
        assert_eq!(format_started_at("000253402300800000"), "253402300800000");
    }

    #[test]
    fn a_size_reads_in_binary_units() {
        for (size_bytes, expected_size) in [
            (512, "512 B"),
            (1023, "1023 B"),
            (2048, "2.0 KiB"),
            (5 * 1024 * 1024, "5.0 MiB"),
            (3 * 1024 * 1024 * 1024, "3.0 GiB"),
            (1536, "1.5 KiB"),
        ] {
            assert_eq!(format_size(size_bytes), expected_size);
        }
    }

    #[test]
    fn an_empty_log_directory_lists_as_one_line_naming_it() {
        assert_eq!(
            render_runtime_log_instance_listing(Path::new("/srv/app/.streamlib/logs"), Vec::new()),
            "(no stream logs in /srv/app/.streamlib/logs)\n"
        );
    }

    #[test]
    fn the_listing_is_newest_started_first_in_three_aligned_columns() {
        let log_directory = Path::new("/logs");

        let runtime_log_listing = render_runtime_log_instance_listing(
            log_directory,
            vec![
                runtime_log_instance(log_directory, "Rolder", "1700000000000", 512),
                runtime_log_instance(log_directory, "Rnewest", "1786136667573", 5 * 1024 * 1024),
                runtime_log_instance(log_directory, "Rstray", "999", 2048),
            ],
        );

        assert_eq!(
            runtime_log_listing,
            [
                "RUNTIME_ID-STREAM         STARTED_AT                SIZE\n",
                "Rnewest                   2026-08-07T21:04:27Z      5.0 MiB\n",
                "Rolder                    2023-11-14T22:13:20Z      512 B\n",
                "Rstray                    1970-01-01T00:00:00Z      2.0 KiB\n",
            ]
            .concat()
        );
    }

    #[test]
    fn list_reads_every_runtime_with_a_log_file_in_the_directory() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1786136667573.jsonl"),
            a_log_line_with_message("active"),
        )
        .unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1786136667573.1.jsonl"),
            a_log_line_with_message("rotated"),
        )
        .unwrap();

        let (printed_listing, notes, printed) = print_stream_log_files_on_disk_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                list_stream_logs_on_disk: true,
                ..Default::default()
            },
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(notes, "");
        let listed_rows: Vec<&str> = printed_listing.lines().collect();
        assert_eq!(listed_rows.len(), 2, "{printed_listing}");
        assert_eq!(
            listed_rows[1].split_whitespace().collect::<Vec<_>>()[..2],
            ["Rabc", "2026-08-07T21:04:27Z"]
        );
    }

    #[test]
    fn list_refuses_the_flags_it_would_otherwise_ignore() {
        let log_directory = tempfile::tempdir().unwrap();

        let (printed_listing, _, printed) = print_stream_log_files_on_disk_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rabc".to_owned()),
                list_stream_logs_on_disk: true,
                follow_appended_records: true,
                record_filters: RuntimeLogRecordFilters {
                    processor_id: Some("proc".to_owned()),
                    pipeline_id: Some("pipe".to_owned()),
                    rhi_operations_only: true,
                    minimum_level: Some(LogLevel::Warn),
                    source: Some(Source::Python),
                    intercepted_only: true,
                },
            },
        );

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(printed),
            "`--list` enumerates the stream logs on disk and reads none of them, so it takes no \
             RUNTIME_ID-STREAM, --follow, --processor, --pipeline, --rhi, --level, --source, \
             --intercepted-only."
        );
        assert_eq!(printed_listing, "");
    }

    #[test]
    fn a_missing_stream_log_instance_name_names_list_and_stream() {
        let log_directory = tempfile::tempdir().unwrap();

        let (_, _, printed) = print_stream_log_files_on_disk_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest::default(),
        );

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(printed),
            "missing RUNTIME_ID-STREAM.\n`tatolab logs --list` enumerates the stream logs on \
             disk, and `--stream` reads a loaded stream's records from the runtime instead."
        );
    }

    #[test]
    fn a_stream_log_with_no_segment_names_list() {
        let log_directory = tempfile::tempdir().unwrap();

        let (_, _, printed) = print_stream_log_files_on_disk_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rnone".to_owned()),
                ..Default::default()
            },
        );

        assert_eq!(
            TatolabCommandFailure::refusal_message_of(printed),
            format!(
                "no stream log `Rnone` in {}.\nA runtime logs per loaded stream, as \
                 `<runtime_id>-<stream>`; `tatolab logs --list` names each one.",
                log_directory.path().display()
            )
        );
    }

    /// A file where the directory should be answers `ENOTDIR` to the listing, which no runtime
    /// writing logs would leave and an empty listing would hide.
    #[test]
    fn an_unreadable_log_directory_is_refused_by_name_rather_than_read_as_empty() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let log_directory_that_is_a_file = scratch_directory.path().join("logs");
        std::fs::write(&log_directory_that_is_a_file, b"not a directory").unwrap();

        for on_disk_request in [
            OnDiskRuntimeLogRequest {
                list_stream_logs_on_disk: true,
                ..Default::default()
            },
            OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rabc".to_owned()),
                ..Default::default()
            },
            OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rabc".to_owned()),
                follow_appended_records: true,
                ..Default::default()
            },
        ] {
            let (printed, _, outcome) = print_stream_log_files_on_disk_capturing_output(
                &log_directory_that_is_a_file,
                &on_disk_request,
            );

            let refusal = TatolabCommandFailure::refusal_message_of(outcome);
            assert!(
                refusal.starts_with(&format!(
                    "cannot read the stream log directory {}: ",
                    log_directory_that_is_a_file.display()
                )),
                "{on_disk_request:?}: {refusal}"
            );
            assert_eq!(printed, "", "{on_disk_request:?}");
        }
    }

    #[test]
    fn reading_a_runtime_prints_its_newest_instances_records_and_exits_zero() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            a_log_line_with_message("old"),
        )
        .unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-2000.jsonl"),
            a_log_line_with_message("new"),
        )
        .unwrap();

        let (printed_records, notes, printed) = print_stream_log_files_on_disk_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rabc".to_owned()),
                ..Default::default()
            },
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(
            printed_records,
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — new\n"
        );
        assert_eq!(notes, "");
    }

    /// The wait's poll is where the file appears; an interrupt after the first record ends the
    /// follow, as Ctrl-C does, with exit 0.
    #[test]
    fn follow_before_the_file_exists_waits_with_its_note_then_reads_it() {
        let log_directory = tempfile::tempdir().unwrap();
        let polls_answered = Cell::new(0_u32);
        let active_segment_path = log_directory.path().join("Rlater-1000.jsonl");
        let read_interrupted = || {
            polls_answered.set(polls_answered.get() + 1);
            if polls_answered.get() == 1 {
                std::fs::write(&active_segment_path, a_log_line_with_message("booted")).unwrap();
            }
            polls_answered.get() > 3
        };
        let mut standard_output = Vec::new();
        let mut standard_error = Vec::new();

        let printed = print_stream_log_files_on_disk(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rlater".to_owned()),
                follow_appended_records: true,
                ..Default::default()
            },
            &mut standard_output,
            &mut standard_error,
            &read_interrupted,
            Duration::ZERO,
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(
            String::from_utf8(standard_error).unwrap(),
            "note: no stream log 'Rlater' yet, waiting in --follow mode...\n"
        );
        assert_eq!(
            String::from_utf8(standard_output).unwrap(),
            "21:04:27.573 [ INFO] [Rabc/rust] tatolabd — booted\n"
        );
    }

    #[test]
    fn an_interrupt_during_the_wait_ends_the_verb_with_exit_zero() {
        let log_directory = tempfile::tempdir().unwrap();
        let mut standard_output = Vec::new();
        let mut standard_error = Vec::new();

        let printed = print_stream_log_files_on_disk(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rnever".to_owned()),
                follow_appended_records: true,
                ..Default::default()
            },
            &mut standard_output,
            &mut standard_error,
            &|| true,
            Duration::ZERO,
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(standard_output, b"");
    }

    /// A pipe whose reader has gone, as `tatolab logs R | head -1` leaves it.
    struct StandardOutputWhoseReaderClosed;

    impl Write for StandardOutputWhoseReaderClosed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_reader_closing_the_pipe_ends_the_read_quietly() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            a_log_line_with_message("unread"),
        )
        .unwrap();
        let mut standard_error = Vec::new();

        let printed = print_stream_log_files_on_disk(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                stream_log_instance_name: Some("Rabc".to_owned()),
                ..Default::default()
            },
            &mut StandardOutputWhoseReaderClosed,
            &mut standard_error,
            &|| false,
            Duration::ZERO,
        );

        assert_eq!(printed.unwrap(), 0);
        assert_eq!(standard_error, b"");
    }
}
