// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The streams the machine's runtime holds, managed one call at a time: `run -d`, `stop`,
//! `start`, `rm`, `streams` and `expose`, each the local API tool of the same purpose, and the
//! one line or table each prints. `run` attached and `dev` share the load request.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use clap::Args;
use serde::de::DeserializeOwned;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ExposePortLevel, ExposePortToolResult, ListStreamsToolResult, ListStreamsToolResultStream,
    RemoveStreamToolResult, RunStreamToolResult, STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT,
    STREAM_LOAD_TOOL_CALL_TIMEOUT, StartStreamToolResult, StopStreamToolResult,
};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::OBSERVATION_VERB_TOOL_CALL_TIMEOUT;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::verb_standard_output::{write_verb_standard_error, write_verb_standard_output};

/// A local API tool that runs, stops or reads the streams the runtime holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamActionTool {
    /// Compiles a project's stream in its own interpreter and loads it.
    RunStream,
    /// Unloads a stream, recording a kept one as stopped.
    StopStream,
    /// Loads a stopped kept stream again.
    StartStream,
    /// Unloads a stream and forgets its record.
    RemoveStream,
    /// Lists the streams the runtime holds.
    ListStreams,
    /// Sets one output port's exposure level.
    ExposePort,
}

impl StreamActionTool {
    /// The tool's name on the local API.
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            StreamActionTool::RunStream => "run_stream",
            StreamActionTool::StopStream => "stop_stream",
            StreamActionTool::StartStream => "start_stream",
            StreamActionTool::RemoveStream => "remove_stream",
            StreamActionTool::ListStreams => "list_streams",
            StreamActionTool::ExposePort => "expose_port",
        }
    }

    /// How long a caller waits for the tool's result: as long as the runtime may take to answer.
    pub(crate) fn tool_call_timeout(self) -> Duration {
        match self {
            StreamActionTool::RunStream | StreamActionTool::StartStream => {
                STREAM_LOAD_TOOL_CALL_TIMEOUT
            }
            StreamActionTool::StopStream
            | StreamActionTool::RemoveStream
            | StreamActionTool::ExposePort => STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT,
            StreamActionTool::ListStreams => OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
        }
    }
}

/// What `streams` prints when the runtime holds none.
const NO_STREAMS_IN_THIS_RUNTIME_LINE: &str = "No streams in this runtime.\n";

/// What `streams` prints in place of a node count for a stream that is not loaded.
const NOT_LOADED_NODE_COUNT_CELL: &str = "-";

/// The flags `run` and `dev` name the stream to load with.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct StreamLoadArguments {
    /// The stream to load: `<file>.py[:<function>]` or `<module>:<function>` (default: the sole
    /// @stream in stream.py).
    #[arg(value_name = "TARGET", conflicts_with = "requested_entry_file")]
    pub(crate) requested_stream_target: Option<OsString>,
    /// Entry file to load, overriding the stream.py convention; not with TARGET.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pub(crate) requested_entry_file: Option<OsString>,
    /// Project directory the runtime compiles the stream in, with its .venv (default: CWD, no
    /// walk-up).
    #[arg(long = "dir", value_name = "DIR")]
    pub(crate) requested_project_directory: Option<OsString>,
    /// Load the stream under this name instead of its function's.
    #[arg(long = "name", value_name = "NAME")]
    pub(crate) requested_stream_name: Option<OsString>,
}

/// What `run` and `dev` ask the runtime to load: a project, the stream function in it, and the
/// name to load it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamLoadRequest {
    /// The absolute project directory the runtime compiles in.
    pub(crate) project_directory: String,
    /// TARGET or FILE verbatim; `None` for the sole @stream in stream.py.
    pub(crate) stream_function: Option<String>,
    /// The name asked for; `None` for the function's own.
    pub(crate) stream_name: Option<String>,
}

impl StreamLoadRequest {
    /// The request `stream_load_arguments` names, its project resolved against
    /// `caller_working_directory`: the canonical `--dir`, else the working directory itself.
    pub(crate) fn from_arguments(
        stream_load_arguments: &StreamLoadArguments,
        caller_working_directory: &Path,
    ) -> Result<Self, TatolabCommandFailure> {
        let project_directory = match &stream_load_arguments.requested_project_directory {
            None => caller_working_directory.to_path_buf(),
            Some(requested_project_directory) => caller_working_directory
                .join(requested_project_directory)
                .canonicalize()
                .ok()
                .filter(|canonical_project_directory| canonical_project_directory.is_dir())
                .ok_or_else(|| {
                    TatolabCommandFailure::refused(format!(
                        "--dir {} is not a directory",
                        Path::new(requested_project_directory).display()
                    ))
                })?,
        };
        Ok(Self {
            project_directory: utf8_text_of(
                "the project directory",
                project_directory.as_os_str(),
            )?,
            stream_function: stream_load_arguments
                .requested_stream_target
                .as_ref()
                .or(stream_load_arguments.requested_entry_file.as_ref())
                .filter(|stream_function| !stream_function.is_empty())
                .map(|stream_function| utf8_text_of("the stream to load", stream_function))
                .transpose()?,
            stream_name: stream_load_arguments
                .requested_stream_name
                .as_ref()
                .filter(|stream_name| !stream_name.is_empty())
                .map(|stream_name| utf8_text_of("--name", stream_name))
                .transpose()?,
        })
    }

    /// `run_stream`'s arguments for this request: kept in the runtime's state directory, or
    /// attached to the connection that carries the call.
    pub(crate) fn run_stream_tool_arguments(
        &self,
        keep: bool,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut run_stream_arguments = serde_json::Map::new();
        run_stream_arguments.insert(
            "project_directory".to_owned(),
            self.project_directory.clone().into(),
        );
        if let Some(stream_function) = &self.stream_function {
            run_stream_arguments
                .insert("stream_function".to_owned(), stream_function.clone().into());
        }
        if let Some(stream_name) = &self.stream_name {
            run_stream_arguments.insert("name".to_owned(), stream_name.clone().into());
        }
        run_stream_arguments.insert("keep".to_owned(), keep.into());
        run_stream_arguments
    }
}

/// `value` as text, refused naming `what_it_names` when it is not UTF-8: the local API carries
/// text.
fn utf8_text_of(
    what_it_names: &str,
    value: &std::ffi::OsStr,
) -> Result<String, TatolabCommandFailure> {
    value.to_str().map(str::to_owned).ok_or_else(|| {
        TatolabCommandFailure::refused(format!(
            "{what_it_names} {} is not UTF-8, which the runtime's local API cannot carry",
            Path::new(value).display()
        ))
    })
}

/// What `run`, `run -d` and `dev` write to their standard error before anything else a load
/// says: each line its compile wrote to its own.
pub(crate) fn rendered_compile_warning_lines(run_stream_result: &RunStreamToolResult) -> String {
    run_stream_result
        .compile_warnings
        .iter()
        .map(|compile_warning| format!("{compile_warning}\n"))
        .collect()
}

/// The answer `tool` gave, read as `ToolResult`, or the refusal naming what it was instead, for
/// each caller to wrap in its own failure.
pub(crate) fn tool_result_from<ToolResult: DeserializeOwned>(
    tool: StreamActionTool,
    tool_result_text: &str,
) -> Result<ToolResult, String> {
    serde_json::from_str(tool_result_text).map_err(|parse_failure| {
        format!(
            "{} answered something other than its result ({parse_failure}): {tool_result_text}",
            tool.tool_name()
        )
    })
}

/// Call `tool` once on the machine's running runtime, bounded by its own timeout, and read its
/// result as `ToolResult`.
fn call_one_stream_action_tool<ToolResult: DeserializeOwned>(
    tool: StreamActionTool,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<ToolResult, TatolabCommandFailure> {
    let tool_result_text = call_one_tool_of_the_running_runtime(
        tool.tool_name(),
        tool_arguments,
        tool.tool_call_timeout(),
    )?;
    tool_result_from(tool, &tool_result_text).map_err(TatolabCommandFailure::refused)
}

/// `count` followed by `noun`, plural unless it is one.
pub(crate) fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `tatolab run -d`: load the stream kept, and print the line saying so.
pub(crate) fn run_stream_kept(
    stream_load_arguments: &StreamLoadArguments,
    caller_working_directory: &Path,
) -> Result<u8, TatolabCommandFailure> {
    let stream_load_request =
        StreamLoadRequest::from_arguments(stream_load_arguments, caller_working_directory)?;
    let run_stream_result: RunStreamToolResult = call_one_stream_action_tool(
        StreamActionTool::RunStream,
        stream_load_request.run_stream_tool_arguments(true),
    )?;
    write_verb_standard_error(&rendered_compile_warning_lines(&run_stream_result));
    write_verb_standard_output(&rendered_kept_stream_line(&run_stream_result))
}

fn rendered_kept_stream_line(run_stream_result: &RunStreamToolResult) -> String {
    format!(
        "{} kept (project {})\n",
        run_stream_result.stream,
        run_stream_result.project_directory.display()
    )
}

/// `{"stream": stream}`, the arguments of every tool that names one stream and nothing else.
fn stream_tool_arguments(stream: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut stream_arguments = serde_json::Map::new();
    stream_arguments.insert("stream".to_owned(), stream.into());
    stream_arguments
}

/// `stop_stream`'s arguments for `stream`.
pub(crate) fn stop_stream_tool_arguments(
    stream: &str,
) -> serde_json::Map<String, serde_json::Value> {
    stream_tool_arguments(stream)
}

/// `tatolab stop STREAM`.
pub(crate) fn stop_stream(stream: &str) -> Result<u8, TatolabCommandFailure> {
    let stop_stream_result: StopStreamToolResult = call_one_stream_action_tool(
        StreamActionTool::StopStream,
        stop_stream_tool_arguments(stream),
    )?;
    if let Some(not_recorded_warning) = stop_not_recorded_warning_line(&stop_stream_result) {
        write_verb_standard_error(&not_recorded_warning);
    }
    write_verb_standard_output(&rendered_stopped_stream_line(&stop_stream_result))
}

fn rendered_stopped_stream_line(stop_stream_result: &StopStreamToolResult) -> String {
    let stream = &stop_stream_result.stream;
    if stop_stream_result.kept && stop_stream_result.not_recorded_because.is_none() {
        format!(
            "{stream} stopped; it stays stopped across restarts until `tatolab start {stream}`\n"
        )
    } else {
        format!("{stream} stopped\n")
    }
}

/// The warning a kept stream stopped but not recorded stopped earns: a restart of the runtime
/// loads it again.
fn stop_not_recorded_warning_line(stop_stream_result: &StopStreamToolResult) -> Option<String> {
    stop_stream_result
        .not_recorded_because
        .as_ref()
        .map(|not_recorded_because| {
            format!(
                "warning: {} was not recorded stopped, so a restart of the runtime loads it \
                 again: {not_recorded_because}\n",
                stop_stream_result.stream
            )
        })
}

/// `tatolab start STREAM`.
pub(crate) fn start_stream(stream: &str) -> Result<u8, TatolabCommandFailure> {
    let start_stream_result: StartStreamToolResult =
        call_one_stream_action_tool(StreamActionTool::StartStream, stream_tool_arguments(stream))?;
    write_verb_standard_output(&format!(
        "{} started ({})\n",
        start_stream_result.stream,
        counted(start_stream_result.node_count, "node")
    ))
}

/// `tatolab rm STREAM`.
pub(crate) fn remove_stream(stream: &str) -> Result<u8, TatolabCommandFailure> {
    let remove_stream_result: RemoveStreamToolResult = call_one_stream_action_tool(
        StreamActionTool::RemoveStream,
        stream_tool_arguments(stream),
    )?;
    write_verb_standard_output(&rendered_removed_stream_line(&remove_stream_result))
}

fn rendered_removed_stream_line(remove_stream_result: &RemoveStreamToolResult) -> String {
    let what_removing_did = match (
        remove_stream_result.unloaded,
        remove_stream_result.forgotten,
    ) {
        (true, true) => " (unloaded and forgotten)",
        (true, false) => " (unloaded)",
        (false, true) => " (forgotten)",
        (false, false) => "",
    };
    format!(
        "{} removed{what_removing_did}\n",
        remove_stream_result.stream
    )
}

/// `tatolab streams`.
pub(crate) fn list_streams() -> Result<u8, TatolabCommandFailure> {
    let list_streams_result: ListStreamsToolResult =
        call_one_stream_action_tool(StreamActionTool::ListStreams, serde_json::Map::new())?;
    write_verb_standard_output(&rendered_streams_table(&list_streams_result.streams))
}

/// The `NAME  STATE  NODES  PROJECT` table, each column as wide as its widest cell; one line when
/// there is no stream.
fn rendered_streams_table(listed_streams: &[ListStreamsToolResultStream]) -> String {
    if listed_streams.is_empty() {
        return NO_STREAMS_IN_THIS_RUNTIME_LINE.to_owned();
    }
    let header_row = [
        "NAME".to_owned(),
        "STATE".to_owned(),
        "NODES".to_owned(),
        "PROJECT".to_owned(),
    ];
    let stream_rows = listed_streams.iter().map(|listed_stream| {
        [
            listed_stream.name.clone(),
            listed_stream.state.to_string(),
            listed_stream.node_count.map_or_else(
                || NOT_LOADED_NODE_COUNT_CELL.to_owned(),
                |node_count| node_count.to_string(),
            ),
            listed_stream.project_directory.display().to_string(),
        ]
    });
    let table_rows: Vec<[String; 4]> = std::iter::once(header_row).chain(stream_rows).collect();
    let column_width = |column_index: usize| {
        table_rows
            .iter()
            .map(|table_row| table_row[column_index].chars().count())
            .max()
            .unwrap_or_default()
    };
    let [name_width, state_width, nodes_width] =
        [column_width(0), column_width(1), column_width(2)];
    table_rows
        .iter()
        .map(|[name, state, nodes, project]| {
            format!(
                "{name:<name_width$}  {state:<state_width$}  {nodes:<nodes_width$}  {project}\n"
            )
        })
        .collect()
}

/// `expose_port`'s arguments for `stream`'s `node`/`port` at `requested_level`.
fn expose_port_tool_arguments(
    stream: &str,
    node: &str,
    port: &str,
    requested_level: ExposePortLevel,
) -> serde_json::Map<String, serde_json::Value> {
    let mut expose_port_arguments = stream_tool_arguments(stream);
    expose_port_arguments.insert("node".to_owned(), node.into());
    expose_port_arguments.insert("port".to_owned(), port.into());
    expose_port_arguments.insert("level".to_owned(), requested_level.wire_spelling().into());
    expose_port_arguments
}

/// `tatolab expose STREAM NODE PORT [--public | --remove]`: `private`, `public` or `internal`.
pub(crate) fn expose_port(
    stream: &str,
    node: &str,
    port: &str,
    requested_level: ExposePortLevel,
) -> Result<u8, TatolabCommandFailure> {
    let expose_port_result: ExposePortToolResult = call_one_stream_action_tool(
        StreamActionTool::ExposePort,
        expose_port_tool_arguments(stream, node, port, requested_level),
    )?;
    if let Some(not_recorded_warning) = exposure_not_recorded_warning_line(&expose_port_result) {
        write_verb_standard_error(&not_recorded_warning);
    }
    write_verb_standard_output(&rendered_exposed_port_line(&expose_port_result))
}

fn rendered_exposed_port_line(expose_port_result: &ExposePortToolResult) -> String {
    let ExposePortToolResult {
        stream,
        node,
        port,
        level,
        recorded,
        not_recorded_because,
    } = expose_port_result;
    let what_holds_it = match (recorded, not_recorded_because) {
        (true, _) => "recorded; it holds across restarts",
        (false, None) => "live only: an attached stream keeps no record",
        (false, Some(_)) => "live only: not recorded",
    };
    format!("{stream}/{node}/{port} is {level} ({what_holds_it})\n")
}

/// The warning a kept stream's level raised live but not recorded earns: a restart of the
/// runtime puts back the level it had. A restriction that cannot be recorded is refused, and
/// exits non-zero as every refusal does.
fn exposure_not_recorded_warning_line(expose_port_result: &ExposePortToolResult) -> Option<String> {
    let ExposePortToolResult {
        stream,
        node,
        port,
        not_recorded_because,
        ..
    } = expose_port_result;
    not_recorded_because.as_ref().map(|not_recorded_because| {
        format!(
            "warning: {stream}/{node}/{port} changed live but was not recorded as the owner's \
             ruling, so a restart of the runtime puts back the level it had: \
             {not_recorded_because}\n"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use streamlib_runtime_client_contract::local_api_wire_contract::LoadedStreamInstance;

    use super::*;

    fn load_arguments(
        requested_stream_target: Option<&str>,
        requested_entry_file: Option<&str>,
        requested_stream_name: Option<&str>,
    ) -> StreamLoadArguments {
        StreamLoadArguments {
            requested_stream_target: requested_stream_target.map(OsString::from),
            requested_entry_file: requested_entry_file.map(OsString::from),
            requested_project_directory: None,
            requested_stream_name: requested_stream_name.map(OsString::from),
        }
    }

    #[test]
    fn the_project_is_the_working_directory_and_the_target_goes_verbatim() {
        let stream_load_request = StreamLoadRequest::from_arguments(
            &load_arguments(Some("stream.py:main"), None, Some("cam")),
            Path::new("/srv/project"),
        )
        .unwrap();

        assert_eq!(
            serde_json::Value::Object(stream_load_request.run_stream_tool_arguments(false)),
            json!({
                "project_directory": "/srv/project",
                "stream_function": "stream.py:main",
                "name": "cam",
                "keep": false,
            })
        );
    }

    #[test]
    fn a_file_is_the_stream_function_and_nothing_named_sends_none() {
        let from_a_file = StreamLoadRequest::from_arguments(
            &load_arguments(None, Some("other.py"), None),
            Path::new("/srv/project"),
        )
        .unwrap();
        let from_nothing = StreamLoadRequest::from_arguments(
            &load_arguments(None, None, Some("")),
            Path::new("/srv/project"),
        )
        .unwrap();

        assert_eq!(
            serde_json::Value::Object(from_a_file.run_stream_tool_arguments(true)),
            json!({"project_directory": "/srv/project", "stream_function": "other.py", "keep": true})
        );
        assert_eq!(
            serde_json::Value::Object(from_nothing.run_stream_tool_arguments(true)),
            json!({"project_directory": "/srv/project", "keep": true})
        );
    }

    #[test]
    fn dir_is_canonicalised_and_refused_when_it_is_not_a_directory() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let project_directory = scratch_directory.path().join("project");
        std::fs::create_dir(&project_directory).unwrap();
        let mut stream_load_arguments = load_arguments(None, None, None);
        stream_load_arguments.requested_project_directory = Some(OsString::from("./project/."));

        assert_eq!(
            StreamLoadRequest::from_arguments(&stream_load_arguments, scratch_directory.path())
                .unwrap()
                .project_directory,
            project_directory.canonicalize().unwrap().to_str().unwrap()
        );

        stream_load_arguments.requested_project_directory = Some(OsString::from("absent"));
        assert_eq!(
            TatolabCommandFailure::refusal_message_of(StreamLoadRequest::from_arguments(
                &stream_load_arguments,
                scratch_directory.path()
            )),
            "--dir absent is not a directory"
        );
    }

    fn run_stream_result_warning(compile_warnings: &[&str]) -> RunStreamToolResult {
        RunStreamToolResult {
            stream: "camera".to_owned(),
            stream_instance: LoadedStreamInstance("4".to_owned()),
            kept: true,
            project_directory: PathBuf::from("/srv/project"),
            node_count: 3,
            replaced_the_kept_record: false,
            compile_warnings: compile_warnings
                .iter()
                .map(|compile_warning| compile_warning.to_string())
                .collect(),
        }
    }

    #[test]
    fn each_stream_action_tool_waits_for_the_bound_of_its_kind() {
        for loading_tool in [StreamActionTool::RunStream, StreamActionTool::StartStream] {
            assert_eq!(
                loading_tool.tool_call_timeout(),
                STREAM_LOAD_TOOL_CALL_TIMEOUT,
                "{loading_tool:?}"
            );
        }
        for tool_that_loads_nothing in [
            StreamActionTool::StopStream,
            StreamActionTool::RemoveStream,
            StreamActionTool::ExposePort,
        ] {
            assert_eq!(
                tool_that_loads_nothing.tool_call_timeout(),
                STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT,
                "{tool_that_loads_nothing:?}"
            );
        }
        assert_eq!(
            StreamActionTool::ListStreams.tool_call_timeout(),
            OBSERVATION_VERB_TOOL_CALL_TIMEOUT
        );
    }

    #[test]
    fn the_kept_line_names_the_stream_and_its_project() {
        assert_eq!(
            rendered_kept_stream_line(&run_stream_result_warning(&[])),
            "camera kept (project /srv/project)\n"
        );
    }

    #[test]
    fn each_compile_warning_is_a_line_of_its_own_and_none_writes_nothing() {
        assert_eq!(
            rendered_compile_warning_lines(&run_stream_result_warning(&[
                "tatolab: the cross-floor check found 1 thing binding this app to one floor.",
                "  processors/effect.py:4: imports `cupy`",
            ])),
            "tatolab: the cross-floor check found 1 thing binding this app to one floor.\n  \
             processors/effect.py:4: imports `cupy`\n"
        );
        assert_eq!(
            rendered_compile_warning_lines(&run_stream_result_warning(&[])),
            ""
        );
    }

    #[test]
    fn a_run_stream_result_without_compile_warnings_is_not_a_run_stream_result() {
        assert!(
            serde_json::from_str::<RunStreamToolResult>(
                r#"{"stream": "camera", "stream_instance": "4", "kept": true, "project_directory": "/srv/project", "node_count": 3, "replaced_the_kept_record": false}"#
            )
            .is_err(),
            "the runtime always answers `compile_warnings`, empty when the compile wrote nothing"
        );
    }

    #[test]
    fn stop_says_whether_the_stream_stays_stopped() {
        assert_eq!(
            rendered_stopped_stream_line(&StopStreamToolResult {
                stream: "camera".to_owned(),
                stopped: true,
                kept: true,
                not_recorded_because: None,
            }),
            "camera stopped; it stays stopped across restarts until `tatolab start camera`\n"
        );
        assert_eq!(
            rendered_stopped_stream_line(&StopStreamToolResult {
                stream: "camera".to_owned(),
                stopped: true,
                kept: false,
                not_recorded_because: None,
            }),
            "camera stopped\n"
        );
    }

    #[test]
    fn a_kept_stop_the_runtime_could_not_record_warns_naming_why() {
        let stop_stream_result: StopStreamToolResult = serde_json::from_value(json!({
            "stream": "camera",
            "stopped": true,
            "kept": true,
            "not_recorded_because": "the record /state/streams/camera.json cannot be read",
        }))
        .unwrap();

        assert_eq!(
            rendered_stopped_stream_line(&stop_stream_result),
            "camera stopped\n",
            "a stop not recorded never claims the stream stays stopped across restarts"
        );
        assert_eq!(
            stop_not_recorded_warning_line(&stop_stream_result).as_deref(),
            Some(
                "warning: camera was not recorded stopped, so a restart of the runtime loads it \
                 again: the record /state/streams/camera.json cannot be read\n"
            )
        );
    }

    #[test]
    fn rm_says_what_removing_did() {
        for (unloaded, forgotten, expected_line) in [
            (true, true, "camera removed (unloaded and forgotten)\n"),
            (true, false, "camera removed (unloaded)\n"),
            (false, true, "camera removed (forgotten)\n"),
        ] {
            assert_eq!(
                rendered_removed_stream_line(&RemoveStreamToolResult {
                    stream: "camera".to_owned(),
                    unloaded,
                    forgotten,
                }),
                expected_line
            );
        }
    }

    #[test]
    fn the_streams_table_aligns_its_columns_and_marks_a_stream_not_loaded() {
        let list_streams_result: ListStreamsToolResult = serde_json::from_value(json!({
            "streams": [
                {"name": "camera", "state": "attached", "project_directory": "/srv/cam", "node_count": 4},
                {"name": "a", "state": "stopped", "project_directory": "/srv/a", "node_count": null},
            ]
        }))
        .unwrap();

        assert_eq!(
            rendered_streams_table(&list_streams_result.streams),
            "NAME    STATE     NODES  PROJECT\n\
             camera  attached  4      /srv/cam\n\
             a       stopped   -      /srv/a\n"
        );
        assert_eq!(rendered_streams_table(&[]), "No streams in this runtime.\n");
    }

    #[test]
    fn expose_sends_the_level_each_flag_names() {
        for (requested_level, wire_level) in [
            (ExposePortLevel::Private, "private"),
            (ExposePortLevel::Public, "public"),
            (ExposePortLevel::Internal, "internal"),
        ] {
            assert_eq!(
                serde_json::Value::Object(expose_port_tool_arguments(
                    "camera",
                    "effect",
                    "video",
                    requested_level
                )),
                json!({"stream": "camera", "node": "effect", "port": "video", "level": wire_level})
            );
        }
    }

    #[test]
    fn the_expose_line_says_whether_the_ruling_was_recorded() {
        let exposed = |recorded| ExposePortToolResult {
            stream: "camera".to_owned(),
            node: "effect".to_owned(),
            port: "video".to_owned(),
            level: ExposePortLevel::Public,
            recorded,
            not_recorded_because: None,
        };

        assert_eq!(
            rendered_exposed_port_line(&exposed(true)),
            "camera/effect/video is public (recorded; it holds across restarts)\n"
        );
        assert_eq!(
            rendered_exposed_port_line(&exposed(false)),
            "camera/effect/video is public (live only: an attached stream keeps no record)\n"
        );
        assert_eq!(exposure_not_recorded_warning_line(&exposed(true)), None);
    }

    #[test]
    fn a_kept_exposure_the_runtime_could_not_record_warns_naming_why() {
        let expose_port_result: ExposePortToolResult = serde_json::from_value(json!({
            "stream": "camera",
            "node": "effect",
            "port": "video",
            "level": "internal",
            "recorded": false,
            "not_recorded_because": "the record /state/streams/camera.json cannot be written",
        }))
        .unwrap();

        assert_eq!(
            rendered_exposed_port_line(&expose_port_result),
            "camera/effect/video is internal (live only: not recorded)\n"
        );
        assert_eq!(
            exposure_not_recorded_warning_line(&expose_port_result).as_deref(),
            Some(
                "warning: camera/effect/video changed live but was not recorded as the owner's \
                 ruling, so a restart of the runtime puts back the level it had: the record \
                 /state/streams/camera.json cannot be written\n"
            )
        );
    }

    #[test]
    fn an_answer_that_is_not_the_tools_result_is_refused_naming_the_tool() {
        let refusal =
            tool_result_from::<StartStreamToolResult>(StreamActionTool::StartStream, "{}")
                .unwrap_err();

        assert!(
            refusal.starts_with("start_stream answered something other than its result ("),
            "{refusal}"
        );
    }
}
