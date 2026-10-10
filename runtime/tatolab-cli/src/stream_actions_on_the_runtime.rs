// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The streams the machine's runtime holds, managed one call at a time: `run -d`, `stop`,
//! `start`, `rm`, `streams` and `expose`, each the local API tool of the same purpose, and the
//! one line or table each prints. `run` attached and `dev` share the load request.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Deserialize;

use crate::TatolabCommandFailure;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::verb_standard_output::{write_verb_standard_error, write_verb_standard_output};

/// The local API tool that compiles a project's stream in its own interpreter and loads it.
pub(crate) const RUN_STREAM_TOOL_NAME: &str = "run_stream";

/// The local API tool that unloads a stream, recording a kept one as stopped.
pub(crate) const STOP_STREAM_TOOL_NAME: &str = "stop_stream";

/// The local API tool that loads a stopped kept stream again.
pub(crate) const START_STREAM_TOOL_NAME: &str = "start_stream";

/// The local API tool that unloads a stream and forgets its record.
pub(crate) const REMOVE_STREAM_TOOL_NAME: &str = "remove_stream";

/// The local API tool that lists the streams the runtime holds.
pub(crate) const LIST_STREAMS_TOOL_NAME: &str = "list_streams";

/// The local API tool that sets one output port's exposure level.
pub(crate) const EXPOSE_PORT_TOOL_NAME: &str = "expose_port";

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

/// `run_stream`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RunStreamToolResult {
    /// The name the stream was loaded under.
    pub(crate) stream: String,
    /// The project it was compiled in.
    pub(crate) project_directory: PathBuf,
    /// How many nodes it loaded with.
    pub(crate) node_count: usize,
    /// Each line the compile wrote to its standard error: the cross-floor check's warnings among
    /// them.
    pub(crate) compile_warnings: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct StopStreamToolResult {
    stream: String,
    kept: bool,
    #[serde(default)]
    not_recorded_because: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct StartStreamToolResult {
    stream: String,
    node_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RemoveStreamToolResult {
    stream: String,
    unloaded: bool,
    forgotten: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ListStreamsToolResult {
    streams: Vec<ListedStream>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ListedStream {
    name: String,
    state: String,
    project_directory: PathBuf,
    node_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ExposePortToolResult {
    stream: String,
    node: String,
    port: String,
    level: String,
    recorded: bool,
    #[serde(default)]
    not_recorded_because: Option<String>,
}

/// The exposure level `expose` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestedPortExposureLevel {
    /// `expose`: readable by this machine's other streams and agents.
    Private,
    /// `expose --public`: readable from off the machine too.
    Public,
    /// `expose --remove`: readable inside its own stream only.
    Internal,
}

impl RequestedPortExposureLevel {
    fn wire_spelling(self) -> &'static str {
        match self {
            RequestedPortExposureLevel::Private => "private",
            RequestedPortExposureLevel::Public => "public",
            RequestedPortExposureLevel::Internal => "internal",
        }
    }
}

/// The answer `tool_name` gave, read as `ToolResult`, or the refusal naming what it was instead.
fn tool_result_from<ToolResult: serde::de::DeserializeOwned>(
    tool_name: &str,
    tool_result_text: &str,
) -> Result<ToolResult, TatolabCommandFailure> {
    serde_json::from_str(tool_result_text).map_err(|parse_failure| {
        TatolabCommandFailure::refused(format!(
            "{tool_name} answered something other than its result ({parse_failure}): \
             {tool_result_text}"
        ))
    })
}

/// `count` followed by `noun`, plural unless it is one.
fn counted(count: usize, noun: &str) -> String {
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
    let run_stream_result: RunStreamToolResult = tool_result_from(
        RUN_STREAM_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(
            RUN_STREAM_TOOL_NAME,
            stream_load_request.run_stream_tool_arguments(true),
        )?,
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
    let stop_stream_result: StopStreamToolResult = tool_result_from(
        STOP_STREAM_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(
            STOP_STREAM_TOOL_NAME,
            stop_stream_tool_arguments(stream),
        )?,
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
    let start_stream_result: StartStreamToolResult = tool_result_from(
        START_STREAM_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(
            START_STREAM_TOOL_NAME,
            stream_tool_arguments(stream),
        )?,
    )?;
    write_verb_standard_output(&format!(
        "{} started ({})\n",
        start_stream_result.stream,
        counted(start_stream_result.node_count, "node")
    ))
}

/// `tatolab rm STREAM`.
pub(crate) fn remove_stream(stream: &str) -> Result<u8, TatolabCommandFailure> {
    let remove_stream_result: RemoveStreamToolResult = tool_result_from(
        REMOVE_STREAM_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(
            REMOVE_STREAM_TOOL_NAME,
            stream_tool_arguments(stream),
        )?,
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
    let list_streams_result: ListStreamsToolResult = tool_result_from(
        LIST_STREAMS_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(LIST_STREAMS_TOOL_NAME, serde_json::Map::new())?,
    )?;
    write_verb_standard_output(&rendered_streams_table(&list_streams_result.streams))
}

/// The `NAME  STATE  NODES  PROJECT` table, each column as wide as its widest cell; one line when
/// there is no stream.
fn rendered_streams_table(listed_streams: &[ListedStream]) -> String {
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
            listed_stream.state.clone(),
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
    requested_level: RequestedPortExposureLevel,
) -> serde_json::Map<String, serde_json::Value> {
    let mut expose_port_arguments = stream_tool_arguments(stream);
    expose_port_arguments.insert("node".to_owned(), node.into());
    expose_port_arguments.insert("port".to_owned(), port.into());
    expose_port_arguments.insert("level".to_owned(), requested_level.wire_spelling().into());
    expose_port_arguments
}

/// `tatolab expose STREAM NODE PORT [--public | --remove]`.
pub(crate) fn expose_port(
    stream: &str,
    node: &str,
    port: &str,
    requested_level: RequestedPortExposureLevel,
) -> Result<u8, TatolabCommandFailure> {
    let expose_port_result: ExposePortToolResult = tool_result_from(
        EXPOSE_PORT_TOOL_NAME,
        &call_one_tool_of_the_running_runtime(
            EXPOSE_PORT_TOOL_NAME,
            expose_port_tool_arguments(stream, node, port, requested_level),
        )?,
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

/// The warning a kept stream's level changed live but not recorded earns: a restart of the
/// runtime puts back the level it had.
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
    use serde_json::json;

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

    #[test]
    fn the_kept_line_names_the_stream_and_its_project() {
        assert_eq!(
            rendered_kept_stream_line(&RunStreamToolResult {
                stream: "camera".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
                node_count: 3,
                compile_warnings: Vec::new(),
            }),
            "camera kept (project /srv/project)\n"
        );
    }

    #[test]
    fn each_compile_warning_is_a_line_of_its_own_and_none_writes_nothing() {
        let run_stream_result_warning = |compile_warnings: &[&str]| RunStreamToolResult {
            stream: "camera".to_owned(),
            project_directory: PathBuf::from("/srv/project"),
            node_count: 3,
            compile_warnings: compile_warnings
                .iter()
                .map(|compile_warning| compile_warning.to_string())
                .collect(),
        };

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
                r#"{"stream": "camera", "project_directory": "/srv/project", "node_count": 3}"#
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
                kept: true,
                not_recorded_because: None,
            }),
            "camera stopped; it stays stopped across restarts until `tatolab start camera`\n"
        );
        assert_eq!(
            rendered_stopped_stream_line(&StopStreamToolResult {
                stream: "camera".to_owned(),
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
            (RequestedPortExposureLevel::Private, "private"),
            (RequestedPortExposureLevel::Public, "public"),
            (RequestedPortExposureLevel::Internal, "internal"),
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
            level: "public".to_owned(),
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
        let refusal = TatolabCommandFailure::refusal_message_of(tool_result_from::<
            StartStreamToolResult,
        >("start_stream", "{}"));

        assert!(
            refusal.starts_with("start_stream answered something other than its result ("),
            "{refusal}"
        );
    }
}
