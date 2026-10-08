// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs`: with RUNTIME_ID, a runtime's on-disk JSONL log rendered as the runtime
//! mirrored it; with `--list`, the runtimes that have one; with `--node`, a bounded sample of a
//! running runtime's live event stream.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Datelike};
use clap::Args;
use clap::builder::PossibleValue;
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, Source};
use streamlib_runtime_client_contract::runtime_log_file_paths::{
    RuntimeLogInstanceOnDisk, log_dir, newest_runtime_log_instance_in_directory,
    runtime_log_instances_in_directory,
};

use crate::runtime_log_files_reader::{
    RUNTIME_LOG_FOLLOW_POLL_INTERVAL, RuntimeLogReadStep, RuntimeLogRecordFilters,
    RuntimeLogRecordsReader,
};
use crate::runtime_observation_verbs::print_local_api_tool_result_of_selected_runtime;
use crate::{RuntimeTargetArguments, TatolabCommandFailure};

/// The local API tool `logs --node` drives.
pub(crate) const LOGS_TOOL_NAME: &str = "logs";

/// The width `--list` pads its RUNTIME_ID and STARTED_AT columns to.
const RUNTIME_LOG_LISTING_COLUMN_WIDTH: usize = 24;

/// The latest year an ISO-8601 STARTED_AT is written for; a later start is shown as its number.
const LATEST_STARTED_AT_YEAR_RENDERED_AS_A_DATE: i32 = 9999;

/// `tatolab logs`' flags.
#[derive(Args, Debug)]
pub(crate) struct RuntimeLogsVerbArguments {
    /// Runtime to read logs for. Omit with --list or --node.
    #[arg(value_name = "RUNTIME_ID")]
    pub(crate) runtime_id: Option<String>,
    /// Enumerate the runtimes that have log files instead of reading one.
    #[arg(long = "list")]
    pub(crate) list_runtimes_with_log_files: bool,
    /// Follow the log file as new records land (like `tail -F`).
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
    #[arg(long = "level", value_name = "LEVEL")]
    pub(crate) minimum_level: Option<MinimumLogLevelArgument>,
    /// Only records emitted by this runtime language.
    #[arg(long = "source", value_name = "SOURCE")]
    pub(crate) source: Option<LogSourceArgument>,
    /// Only intercepted records (captured stdout/stderr/print).
    #[arg(long = "intercepted-only")]
    pub(crate) intercepted_only: bool,
    /// (--node only) Max events to collect before returning.
    #[arg(long = "count", value_name = "N", allow_negative_numbers = true)]
    pub(crate) requested_event_count: Option<i64>,
    #[command(flatten)]
    pub(crate) runtime_target: RuntimeTargetArguments,
}

/// `--level`'s value: a [`LogLevel`] spelled as the JSONL record spells it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MinimumLogLevelArgument(LogLevel);

impl clap::ValueEnum for MinimumLogLevelArgument {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self(LogLevel::Trace),
            Self(LogLevel::Debug),
            Self(LogLevel::Info),
            Self(LogLevel::Warn),
            Self(LogLevel::Error),
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.0.as_str()))
    }
}

/// `--source`'s value: a [`Source`] spelled as the JSONL record spells it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LogSourceArgument(Source);

impl clap::ValueEnum for LogSourceArgument {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self(Source::Rust), Self(Source::Python)]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.0.as_str()))
    }
}

/// What `logs` was asked to do on disk, once `--node` and `--count` are ruled out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OnDiskRuntimeLogRequest {
    /// The runtime whose newest instance is read.
    pub(crate) runtime_id: Option<String>,
    /// List the runtimes with log files rather than read one.
    pub(crate) list_runtimes_with_log_files: bool,
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
            ("RUNTIME_ID", is_given(&self.runtime_id)),
            ("--list", self.list_runtimes_with_log_files),
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
}

fn is_given(flag_value: &Option<String>) -> bool {
    flag_value
        .as_deref()
        .is_some_and(|flag_value| !flag_value.is_empty())
}

impl From<&RuntimeLogsVerbArguments> for OnDiskRuntimeLogRequest {
    fn from(logs_arguments: &RuntimeLogsVerbArguments) -> Self {
        Self {
            runtime_id: logs_arguments.runtime_id.clone(),
            list_runtimes_with_log_files: logs_arguments.list_runtimes_with_log_files,
            follow_appended_records: logs_arguments.follow_appended_records,
            record_filters: RuntimeLogRecordFilters {
                processor_id: logs_arguments.processor_id.clone(),
                pipeline_id: logs_arguments.pipeline_id.clone(),
                rhi_operations_only: logs_arguments.rhi_operations_only,
                minimum_level: logs_arguments
                    .minimum_level
                    .map(|minimum_level| minimum_level.0),
                source: logs_arguments.source.map(|source| source.0),
                intercepted_only: logs_arguments.intercepted_only,
            },
        }
    }
}

/// `tatolab logs`: `--node` picks the live event stream, anything else reads the disk.
///
/// The on-disk flags have no meaning against a live event stream (the tool takes a count and
/// nothing else), so asking for both is refused rather than silently ignoring the flag.
pub(crate) fn run_runtime_logs_verb(
    logs_arguments: RuntimeLogsVerbArguments,
) -> Result<u8, TatolabCommandFailure> {
    let on_disk_request = OnDiskRuntimeLogRequest::from(&logs_arguments);
    if let Some(requested_runtime_name_or_id) = logs_arguments
        .runtime_target
        .requested_runtime_name_or_id
        .as_deref()
        .filter(|requested_runtime_name_or_id| !requested_runtime_name_or_id.is_empty())
    {
        let on_disk_flags_given = on_disk_request.on_disk_flags_given();
        if !on_disk_flags_given.is_empty() {
            return Err(TatolabCommandFailure::refused(format!(
                "`--node` reads a running runtime's live event stream, which takes no {}. Drop \
                 `--node` to read an on-disk log file instead.",
                on_disk_flags_given.join(", ")
            )));
        }
        return print_local_api_tool_result_of_selected_runtime(
            Some(requested_runtime_name_or_id),
            LOGS_TOOL_NAME,
            logs_tool_arguments(logs_arguments.requested_event_count),
        );
    }
    if logs_arguments.requested_event_count.is_some() {
        return Err(TatolabCommandFailure::refused(
            "`--count` bounds a live event-stream sample; it has no meaning for an on-disk log \
             file. Use `--node`, or drop `--count`."
                .to_owned(),
        ));
    }
    end_the_read_on_interrupt().map_err(|interrupt_routing_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot route Ctrl-C to the end of the read: {interrupt_routing_failure}"
        ))
    })?;
    let standard_output = io::stdout();
    let mut buffered_standard_output = io::BufWriter::new(standard_output.lock());
    print_runtime_log_files(
        &log_dir(),
        &on_disk_request,
        &mut buffered_standard_output,
        &mut io::stderr(),
        &|| INTERRUPT_DELIVERED_DURING_THE_READ.load(Ordering::SeqCst),
        RUNTIME_LOG_FOLLOW_POLL_INTERVAL,
    )
}

/// The `logs` tool's arguments: a count only when one was asked for and is not zero, so the
/// tool's own default applies otherwise.
pub(crate) fn logs_tool_arguments(
    requested_event_count: Option<i64>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut logs_arguments = serde_json::Map::new();
    if let Some(requested_event_count) = requested_event_count.filter(|&count| count != 0) {
        logs_arguments.insert("count".to_owned(), requested_event_count.into());
    }
    logs_arguments
}

/// The on-disk side of `logs` against the log files in `log_directory`: `--list`, or one
/// runtime's records — followed until `read_interrupted` answers true when following.
pub(crate) fn print_runtime_log_files(
    log_directory: &Path,
    on_disk_request: &OnDiskRuntimeLogRequest,
    standard_output: &mut dyn Write,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Result<u8, TatolabCommandFailure> {
    if on_disk_request.list_runtimes_with_log_files {
        let flags_ignored_beside_list = on_disk_request.flags_given_besides_list();
        if !flags_ignored_beside_list.is_empty() {
            return Err(TatolabCommandFailure::refused(format!(
                "`--list` enumerates the runtimes that have log files and reads none of them, so \
                 it takes no {}.",
                flags_ignored_beside_list.join(", ")
            )));
        }
        let runtime_log_listing = render_runtime_log_instance_listing(
            log_directory,
            runtime_log_instances_in_directory(log_directory),
        );
        return match standard_output
            .write_all(runtime_log_listing.as_bytes())
            .and_then(|()| standard_output.flush())
        {
            Ok(()) => Ok(0),
            Err(write_failure) => standard_output_closed_or_failed(write_failure),
        };
    }
    let Some(runtime_id) = on_disk_request.runtime_id.as_deref() else {
        return Err(TatolabCommandFailure::refused(
            "missing RUNTIME_ID.\n`tatolab logs --list` enumerates the runtimes that have log \
             files, and `--node` reads a running runtime's live event stream instead."
                .to_owned(),
        ));
    };
    let instance_to_read = match newest_runtime_log_instance_in_directory(log_directory, runtime_id)
    {
        Some(instance_to_read) => instance_to_read,
        None if !on_disk_request.follow_appended_records => {
            return Err(TatolabCommandFailure::refused(format!(
                "no log file for runtime `{runtime_id}` in {}.\nUse `tatolab logs --list` to see \
                 the runtimes that have one.",
                log_directory.display()
            )));
        }
        None => match wait_for_the_first_log_file_of_runtime(
            log_directory,
            runtime_id,
            standard_error,
            read_interrupted,
            follow_poll_interval,
        ) {
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
    loop {
        if read_interrupted() {
            break;
        }
        let read_step = runtime_log_records_reader
            .next_step(standard_error)
            .map_err(|segment_read_failure| {
                let _ = standard_output.flush();
                TatolabCommandFailure::refused(segment_read_failure.to_string())
            })?;
        let written = match read_step {
            RuntimeLogReadStep::RenderedRecord(rendered_record) => {
                standard_output.write_all(rendered_record.as_bytes())
            }
            RuntimeLogReadStep::LiveEdgeReached => standard_output.flush().map(|()| {
                std::thread::sleep(follow_poll_interval);
            }),
            RuntimeLogReadStep::Finished => break,
        };
        if let Err(write_failure) = written {
            return standard_output_closed_or_failed(write_failure);
        }
    }
    match standard_output.flush() {
        Ok(()) => Ok(0),
        Err(write_failure) => standard_output_closed_or_failed(write_failure),
    }
}

/// A reader that closed its end of the pipe has seen all it wanted, which ends the read
/// quietly; any other failure to write is the verb's.
fn standard_output_closed_or_failed(write_failure: io::Error) -> Result<u8, TatolabCommandFailure> {
    if write_failure.kind() == io::ErrorKind::BrokenPipe {
        return Ok(0);
    }
    Err(TatolabCommandFailure::refused(format!(
        "cannot write to standard output: {write_failure}"
    )))
}

/// Wait for `runtime_id`'s first log file, for `--follow` before the runtime starts; `None` when
/// the wait was interrupted.
///
/// Following a runtime you are about to start is the point of `--follow`; failing because the
/// file does not exist yet would refuse the one case the flag is for.
fn wait_for_the_first_log_file_of_runtime(
    log_directory: &Path,
    runtime_id: &str,
    standard_error: &mut dyn Write,
    read_interrupted: &dyn Fn() -> bool,
    follow_poll_interval: Duration,
) -> Option<RuntimeLogInstanceOnDisk> {
    let _ = writeln!(
        standard_error,
        "note: no log file yet for runtime '{runtime_id}', waiting in --follow mode..."
    );
    loop {
        if let Some(first_instance) =
            newest_runtime_log_instance_in_directory(log_directory, runtime_id)
        {
            return Some(first_instance);
        }
        if read_interrupted() {
            return None;
        }
        std::thread::sleep(follow_poll_interval);
    }
}

/// What `--list` prints for `runtime_log_instances` found in `log_directory`: newest started
/// first, or one line naming the directory when it holds none.
pub(crate) fn render_runtime_log_instance_listing(
    log_directory: &Path,
    mut runtime_log_instances: Vec<RuntimeLogInstanceOnDisk>,
) -> String {
    if runtime_log_instances.is_empty() {
        return format!("(no runtime log files in {})\n", log_directory.display());
    }
    runtime_log_instances
        .sort_by(|earlier_listed, later_listed| later_listed.compare_started_at(earlier_listed));
    let mut runtime_log_listing = format!(
        "{:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  SIZE\n",
        "RUNTIME_ID", "STARTED_AT"
    );
    for runtime_log_instance in &runtime_log_instances {
        runtime_log_listing.push_str(&format!(
            "{:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {:<RUNTIME_LOG_LISTING_COLUMN_WIDTH$}  {}\n",
            runtime_log_instance.runtime_id,
            format_started_at(&runtime_log_instance.started_at_millis_digits),
            format_size(runtime_log_instance.total_segment_bytes)
        ));
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

/// Set by SIGINT while `logs` reads the disk, so Ctrl-C ends the read — a `--follow` above all —
/// with exit 0 rather than killing the process mid-line.
static INTERRUPT_DELIVERED_DURING_THE_READ: AtomicBool = AtomicBool::new(false);

extern "C" fn record_interrupt_delivered_during_the_read(_delivered_signal: libc::c_int) {
    INTERRUPT_DELIVERED_DURING_THE_READ.store(true, Ordering::SeqCst);
}

/// Route SIGINT to [`INTERRUPT_DELIVERED_DURING_THE_READ`]. A SIGINT inherited as ignored stays
/// ignored, as it does for a Python console script.
fn end_the_read_on_interrupt() -> io::Result<()> {
    // SAFETY: a null `act` is POSIX's read-only query; `inherited_interrupt_disposition` is a
    // zeroed `sigaction` this frame owns for the kernel to write into.
    let inherited_interrupt_disposition = unsafe {
        let mut inherited_interrupt_disposition: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(
            libc::SIGINT,
            std::ptr::null(),
            &mut inherited_interrupt_disposition,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        inherited_interrupt_disposition
    };
    if inherited_interrupt_disposition.sa_sigaction == libc::SIG_IGN {
        return Ok(());
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe; the action is
    // fully initialised before it is installed, and a null old-action pointer is allowed.
    unsafe {
        let mut interrupt_action: libc::sigaction = std::mem::zeroed();
        interrupt_action.sa_sigaction =
            record_interrupt_delivered_during_the_read as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut interrupt_action.sa_mask);
        interrupt_action.sa_flags = libc::SA_RESTART;
        if libc::sigaction(libc::SIGINT, &interrupt_action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use clap::Parser;

    use super::*;

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
    fn a_runtime_target_with_on_disk_flags_is_refused_naming_each_in_help_order() {
        let refused = run_runtime_logs_verb(logs_arguments_parsed_from(&[
            "--level",
            "warn",
            "Rabc",
            "--node",
            "rig-logs",
            "--intercepted-only",
            "--list",
            "-f",
            "--processor",
            "proc",
            "--pipeline",
            "pipe",
            "--rhi",
            "--source",
            "python",
        ]));

        assert_eq!(
            refusal_message(refused),
            "`--node` reads a running runtime's live event stream, which takes no RUNTIME_ID, \
             --list, --follow, --processor, --pipeline, --rhi, --level, --source, \
             --intercepted-only. Drop `--node` to read an on-disk log file instead."
        );
    }

    #[test]
    fn a_count_without_a_runtime_target_is_refused() {
        for logs_flags in [
            &["Rabc", "--count", "5"][..],
            &["Rabc", "--count", "0"],
            &["--list", "--count", "5", "--node", ""],
        ] {
            assert_eq!(
                refusal_message(run_runtime_logs_verb(logs_arguments_parsed_from(
                    logs_flags
                ))),
                "`--count` bounds a live event-stream sample; it has no meaning for an on-disk \
                 log file. Use `--node`, or drop `--count`.",
                "{logs_flags:?}"
            );
        }
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
            active_segment_path: log_directory
                .join(format!("{runtime_id}-{started_at_millis_digits}.jsonl")),
            total_segment_bytes,
        }
    }

    fn a_log_line_with_message(message: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "schema_version": 1,
                "host_ts": 1_786_136_667_573_387_556_u64,
                "runtime_id": "Rabc",
                "source": "rust",
                "level": "info",
                "message": message,
                "target": "tatolabd",
                "intercepted": false,
            })
        )
    }

    /// `print_runtime_log_files` against `log_directory`, never interrupted: stdout, stderr and
    /// the outcome.
    fn print_runtime_log_files_capturing_output(
        log_directory: &Path,
        on_disk_request: &OnDiskRuntimeLogRequest,
    ) -> (String, String, Result<u8, TatolabCommandFailure>) {
        let mut standard_output = Vec::new();
        let mut standard_error = Vec::new();
        let printed = print_runtime_log_files(
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

    fn refusal_message(printed: Result<u8, TatolabCommandFailure>) -> String {
        let command_failure = printed.unwrap_err();
        assert_eq!(command_failure.exit_code, 1);
        command_failure.message_for_the_user.unwrap()
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
            "(no runtime log files in /srv/app/.streamlib/logs)\n"
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
                "RUNTIME_ID                STARTED_AT                SIZE\n",
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

        let (printed_listing, notes, printed) = print_runtime_log_files_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                list_runtimes_with_log_files: true,
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

        let (printed_listing, _, printed) = print_runtime_log_files_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rabc".to_owned()),
                list_runtimes_with_log_files: true,
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
            refusal_message(printed),
            "`--list` enumerates the runtimes that have log files and reads none of them, so it \
             takes no RUNTIME_ID, --follow, --processor, --pipeline, --rhi, --level, --source, \
             --intercepted-only."
        );
        assert_eq!(printed_listing, "");
    }

    #[test]
    fn a_missing_runtime_id_names_list_and_node() {
        let log_directory = tempfile::tempdir().unwrap();

        let (_, _, printed) = print_runtime_log_files_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest::default(),
        );

        assert_eq!(
            refusal_message(printed),
            "missing RUNTIME_ID.\n`tatolab logs --list` enumerates the runtimes that have log \
             files, and `--node` reads a running runtime's live event stream instead."
        );
    }

    #[test]
    fn a_runtime_with_no_log_file_names_list() {
        let log_directory = tempfile::tempdir().unwrap();

        let (_, _, printed) = print_runtime_log_files_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rnone".to_owned()),
                ..Default::default()
            },
        );

        assert_eq!(
            refusal_message(printed),
            format!(
                "no log file for runtime `Rnone` in {}.\nUse `tatolab logs --list` to see the \
                 runtimes that have one.",
                log_directory.path().display()
            )
        );
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

        let (printed_records, notes, printed) = print_runtime_log_files_capturing_output(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rabc".to_owned()),
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

        let printed = print_runtime_log_files(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rlater".to_owned()),
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
            "note: no log file yet for runtime 'Rlater', waiting in --follow mode...\n"
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

        let printed = print_runtime_log_files(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rnever".to_owned()),
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

    #[test]
    fn the_logs_tool_is_sent_a_count_only_when_one_other_than_zero_was_asked_for() {
        assert_eq!(
            serde_json::Value::Object(logs_tool_arguments(Some(4))),
            serde_json::json!({"count": 4})
        );
        assert_eq!(
            serde_json::Value::Object(logs_tool_arguments(Some(0))),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::Value::Object(logs_tool_arguments(None)),
            serde_json::json!({})
        );
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

        let printed = print_runtime_log_files(
            log_directory.path(),
            &OnDiskRuntimeLogRequest {
                runtime_id: Some("Rabc".to_owned()),
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
