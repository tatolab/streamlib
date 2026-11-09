// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The streams the machine's runtime holds, managed one call at a time: `new`, `run`, `stop`,
//! `rm`, `cancel`, `rename`, `streams` and `clock`, each the local API tool of the same purpose, and the
//! lines or table each prints. A verb naming one stream resolves its stream argument to an id
//! through `list_streams` first. `run` and `dev` share the project load request.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CancelStreamActionToolResult, CreateProjectToolResult, ListStreamsToolResult,
    ListStreamsToolResultStream, ListedStreamState, RemoveProjectToolResult,
    RenameStreamToolResult, RunProjectToolResult, SetStreamClockScaleToolResult,
    StopProjectToolResult, StreamActionStateOnTheWire,
};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::ONE_SHOT_VERB_TOOL_CALL_TIMEOUT;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::stream_argument_resolution_through_the_listing::stream_id_the_stream_argument_names_on_the_running_runtime;
use crate::verb_output_table::rendered_padded_table;
use crate::verb_standard_output::write_verb_standard_output;

/// A local API tool that runs, stops or reads the streams the runtime holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamActionTool {
    /// Queues a load of every stream of a project.
    RunProject,
    /// Creates a project and queues its load.
    CreateProject,
    /// Unloads every stream of a project, recording each stopped.
    StopProject,
    /// Unloads every stream of a project and forgets every record of it.
    RemoveProject,
    /// Drops a queued stream action, or stops a running one at its step.
    CancelStreamAction,
    /// Shows a stream under another display name.
    RenameStream,
    /// Lists the streams the runtime holds.
    ListStreams,
    /// Runs a started stream's scaled clock at a new scale.
    SetStreamClockScale,
}

impl StreamActionTool {
    /// The tool's name on the local API.
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            StreamActionTool::RunProject => "run_project",
            StreamActionTool::CreateProject => "create_project",
            StreamActionTool::StopProject => "stop_project",
            StreamActionTool::RemoveProject => "remove_project",
            StreamActionTool::CancelStreamAction => "cancel_stream_action",
            StreamActionTool::RenameStream => "rename_stream",
            StreamActionTool::ListStreams => "list_streams",
            StreamActionTool::SetStreamClockScale => "set_stream_clock_scale",
        }
    }
}

/// The short help of a verb's stream argument.
pub(crate) const STREAM_ARGUMENT_HELP: &str = "The stream: its id, or a display name only it holds \
    (default: the stream loaded from the working directory)";

/// The long help of a verb's stream argument.
pub(crate) const STREAM_ARGUMENT_LONG_HELP: &str = "The stream: a stream id, or a display name \
    exactly one stream holds. Omitted, it is the one stream loaded from the working directory, \
    stopped and failed ones included. An argument several streams match, or none, is refused \
    listing each candidate's id and display name.";

/// What `streams` prints when the runtime holds none.
const NO_STREAMS_IN_THIS_RUNTIME_LINE: &str = "No streams in this runtime.\n";

/// What `streams` prints in place of a node count for a stream that is not loaded.
const NOT_LOADED_NODE_COUNT_CELL: &str = "-";

/// What `streams` prints in place of a step for a preparing stream whose action waits its turn.
const STREAM_ACTION_STILL_QUEUED_CELL: &str = "queued";

/// The `PROJECT` and `ASSET` cells of a stream with no location: one a library host loaded,
/// or a record an earlier runtime wrote.
const NO_LOCATION_CELL: &str = "-";

/// What `run` and `dev` ask the runtime to load: the project, every stream asset of its
/// `streams/` graph documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectLoadRequest {
    /// The absolute project directory the runtime loads, symlinks resolved.
    pub(crate) project_directory: String,
}

impl ProjectLoadRequest {
    /// The request for the project `--dir` names against `caller_working_directory`, else the
    /// working directory itself.
    pub(crate) fn of_the_project_a_verb_names(
        caller_working_directory: &Path,
        requested_project_directory: Option<&Path>,
    ) -> Result<Self, TatolabCommandFailure> {
        let project_directory = the_project_directory_a_verb_names(
            caller_working_directory,
            requested_project_directory,
        )?;
        Ok(Self {
            project_directory: utf8_text_of(
                "the project directory",
                project_directory.as_os_str(),
            )?,
        })
    }

    /// `run_project`'s arguments for this request.
    pub(crate) fn run_project_tool_arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut run_project_arguments = serde_json::Map::new();
        run_project_arguments.insert(
            "project_directory".to_owned(),
            self.project_directory.clone().into(),
        );
        run_project_arguments
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

/// The answer `tool` gave, read as `ToolResult`, or the refusal naming what it was instead, for
/// each caller to wrap in its own failure.
pub(crate) fn tool_result_from<ToolResult: DeserializeOwned>(
    tool_name: &str,
    tool_result_text: &str,
) -> Result<ToolResult, String> {
    serde_json::from_str(tool_result_text).map_err(|parse_failure| {
        format!(
            "{tool_name} answered something other than its result ({parse_failure}): \
             {tool_result_text}"
        )
    })
}

/// Call `tool_name` once on the machine's running runtime, waiting at most `call_timeout`, and
/// read its result as `ToolResult`.
pub(crate) fn call_one_tool_reading_its_result<ToolResult: DeserializeOwned>(
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
    call_timeout: Duration,
) -> Result<ToolResult, TatolabCommandFailure> {
    let tool_result_text =
        call_one_tool_of_the_running_runtime(tool_name, tool_arguments, call_timeout)?;
    tool_result_from(tool_name, &tool_result_text).map_err(TatolabCommandFailure::refused)
}

/// Call `tool` once on the machine's running runtime, bounded as every one-shot verb is, and read
/// its result as `ToolResult`.
pub(crate) fn call_one_stream_action_tool<ToolResult: DeserializeOwned>(
    tool: StreamActionTool,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<ToolResult, TatolabCommandFailure> {
    call_one_tool_reading_its_result(
        tool.tool_name(),
        tool_arguments,
        ONE_SHOT_VERB_TOOL_CALL_TIMEOUT,
    )
}

/// `tatolab run [--dir DIR]`: queue a load of the project, print the action it queued and how to
/// follow it, and return; the load's outcome is in the project's log and in `tatolab streams`.
pub(crate) fn run_project(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<u8, TatolabCommandFailure> {
    let project_load_request = ProjectLoadRequest::of_the_project_a_verb_names(
        caller_working_directory,
        requested_project_directory,
    )?;
    let run_project_result: RunProjectToolResult = call_one_stream_action_tool(
        StreamActionTool::RunProject,
        project_load_request.run_project_tool_arguments(),
    )?;
    write_verb_standard_output(&rendered_run_project_lines(
        &run_project_result,
        caller_working_directory,
    ))
}

/// `tatolab new [DIRECTORY]`: create a project in `requested_project_directory` resolved against
/// `caller_working_directory`, or with none in a folder the runtime draws under `~/streams`,
/// print the action that writes and loads it and how to follow it, and return.
pub(crate) fn create_project(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<u8, TatolabCommandFailure> {
    let mut create_project_arguments = serde_json::Map::new();
    if let Some(requested_project_directory) = requested_project_directory {
        create_project_arguments.insert(
            "project_directory".to_owned(),
            utf8_text_of(
                "the project directory",
                caller_working_directory
                    .join(requested_project_directory)
                    .as_os_str(),
            )?
            .into(),
        );
    }
    let create_project_result: CreateProjectToolResult =
        call_one_stream_action_tool(StreamActionTool::CreateProject, create_project_arguments)?;
    write_verb_standard_output(&rendered_create_project_lines(&create_project_result))
}

/// `<action> queued: create of <project>`, then how to follow it: the project's log by `--dir`.
fn rendered_create_project_lines(create_project_result: &CreateProjectToolResult) -> String {
    let project_directory = create_project_result
        .project_directory
        .display()
        .to_string();
    format!(
        "{}  follow it: tatolab logs -f --dir {project_directory}\n",
        rendered_queued_stream_action_line(
            &create_project_result.action,
            "create",
            &project_directory,
        )
    )
}

/// `<action> queued: load of <project>`, then how to follow it from `caller_working_directory`:
/// the project's log, named with `--dir` when the caller is elsewhere.
fn rendered_run_project_lines(
    run_project_result: &RunProjectToolResult,
    caller_working_directory: &Path,
) -> String {
    let resolved_caller_working_directory = std::fs::canonicalize(caller_working_directory)
        .unwrap_or_else(|_| caller_working_directory.to_path_buf());
    let project_directory = &run_project_result.project_directory;
    let mut follow_it = "  follow it: tatolab logs -f".to_owned();
    if *project_directory != resolved_caller_working_directory {
        follow_it.push_str(&format!(" --dir {}", project_directory.display()));
    }
    format!(
        "{}{follow_it}\n",
        rendered_queued_stream_action_line(
            &run_project_result.action,
            "load",
            &project_directory.display().to_string(),
        )
    )
}

/// `{"stream": stream}`, the arguments of every tool that names one stream and nothing else.
fn stream_tool_arguments(stream: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut stream_arguments = serde_json::Map::new();
    stream_arguments.insert("stream".to_owned(), stream.into());
    stream_arguments
}

/// `tatolab stop [--dir DIR]`: the stop of the whole project — `--dir` resolved against
/// `caller_working_directory`, else the working directory itself.
pub(crate) fn stop_project(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<u8, TatolabCommandFailure> {
    let project_directory =
        the_project_directory_a_verb_names(caller_working_directory, requested_project_directory)?;
    let mut stop_project_arguments = serde_json::Map::new();
    stop_project_arguments.insert(
        "project_directory".to_owned(),
        project_directory.display().to_string().into(),
    );
    let stop_project_result: StopProjectToolResult =
        call_one_stream_action_tool(StreamActionTool::StopProject, stop_project_arguments)?;
    write_verb_standard_output(&rendered_queued_stream_action_line(
        &stop_project_result.action,
        "stop",
        &stop_project_result.project_directory.display().to_string(),
    ))
}

/// `tatolab rm [--dir DIR]`: the removal of the whole project — `--dir` resolved against
/// `caller_working_directory`, else the working directory itself. A `--dir` that no longer
/// exists is passed as given, so the streams of a deleted project can still be forgotten.
pub(crate) fn remove_project(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<u8, TatolabCommandFailure> {
    let project_directory = match requested_project_directory {
        Some(requested_project_directory)
            if !caller_working_directory
                .join(requested_project_directory)
                .exists() =>
        {
            caller_working_directory.join(requested_project_directory)
        }
        _ => the_project_directory_a_verb_names(
            caller_working_directory,
            requested_project_directory,
        )?,
    };
    let mut remove_project_arguments = serde_json::Map::new();
    remove_project_arguments.insert(
        "project_directory".to_owned(),
        project_directory.display().to_string().into(),
    );
    let remove_project_result: RemoveProjectToolResult =
        call_one_stream_action_tool(StreamActionTool::RemoveProject, remove_project_arguments)?;
    write_verb_standard_output(&rendered_queued_stream_action_line(
        &remove_project_result.action,
        "remove",
        &remove_project_result
            .project_directory
            .display()
            .to_string(),
    ))
}

/// `tatolab cancel <ACTION>`: the action's state once the runtime took the cancel.
pub(crate) fn cancel_stream_action(action: &str) -> Result<u8, TatolabCommandFailure> {
    let mut cancel_stream_action_arguments = serde_json::Map::new();
    cancel_stream_action_arguments.insert("action".to_owned(), action.into());
    let cancel_stream_action_result: CancelStreamActionToolResult = call_one_stream_action_tool(
        StreamActionTool::CancelStreamAction,
        cancel_stream_action_arguments,
    )?;
    write_verb_standard_output(&rendered_cancelled_stream_action_line(
        &cancel_stream_action_result,
    ))
}

/// The line `cancel` prints: `<action> cancelled`, or for a running action
/// `<action> cancelling`, with what happens next.
fn rendered_cancelled_stream_action_line(cancelled: &CancelStreamActionToolResult) -> String {
    match cancelled.state {
        StreamActionStateOnTheWire::InProgress => format!(
            "{} cancelling: it stops at the step it is on, then ends cancelled\n",
            cancelled.action
        ),
        state => format!("{} {}\n", cancelled.action, state.wire_spelling()),
    }
}

/// `<action> queued: <verb> of <what>`, the line a verb whose stream action is queued prints.
pub(crate) fn rendered_queued_stream_action_line(
    action: &str,
    verb: &str,
    what_it_acts_on: &str,
) -> String {
    format!("{action} queued: {verb} of {what_it_acts_on}\n")
}

/// The canonical project directory a verb names: `--dir` resolved against
/// `caller_working_directory`, else the working directory itself.
pub(crate) fn the_project_directory_a_verb_names(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<PathBuf, TatolabCommandFailure> {
    let project_directory =
        caller_working_directory.join(requested_project_directory.unwrap_or(Path::new("")));
    project_directory
        .canonicalize()
        .ok()
        .filter(|canonical_project_directory| canonical_project_directory.is_dir())
        .ok_or_else(|| {
            TatolabCommandFailure::refused(format!(
                "{} is not a directory",
                requested_project_directory.map_or_else(
                    || format!("the working directory {}", project_directory.display()),
                    |requested_project_directory| format!(
                        "--dir {}",
                        requested_project_directory.display()
                    ),
                )
            ))
        })
}

/// `tatolab rename [STREAM] DISPLAY_NAME`, its positionals as given.
pub(crate) fn rename_stream(
    stream_then_display_name: &[String],
) -> Result<u8, TatolabCommandFailure> {
    let (stream_argument, display_name) = match stream_then_display_name {
        [display_name] => (None, display_name),
        [stream, display_name] => (Some(stream.as_str()), display_name),
        _ => {
            return Err(TatolabCommandFailure::refused(
                "rename takes [STREAM] DISPLAY_NAME".to_owned(),
            ));
        }
    };
    let stream = stream_id_the_stream_argument_names_on_the_running_runtime(stream_argument)?;
    let mut rename_stream_arguments = stream_tool_arguments(&stream);
    rename_stream_arguments.insert("display_name".to_owned(), display_name.as_str().into());
    let rename_stream_result: RenameStreamToolResult =
        call_one_stream_action_tool(StreamActionTool::RenameStream, rename_stream_arguments)?;
    write_verb_standard_output(&format!(
        "{} is shown as \"{}\"\n",
        rename_stream_result.stream, rename_stream_result.display_name
    ))
}

/// `tatolab streams`.
pub(crate) fn list_streams() -> Result<u8, TatolabCommandFailure> {
    let list_streams_result: ListStreamsToolResult =
        call_one_stream_action_tool(StreamActionTool::ListStreams, serde_json::Map::new())?;
    write_verb_standard_output(&rendered_streams_table(&list_streams_result.streams))
}

/// The `ID  NAME  STATE  NODES  PROJECT  ASSET` table, each column as wide as its widest cell,
/// then an `<id> <step>` line per preparing stream, a line per failed stream saying why it
/// failed and a line per stream waiting for capture-device access nobody has asked for; one line
/// when there is no stream.
fn rendered_streams_table(listed_streams: &[ListStreamsToolResultStream]) -> String {
    if listed_streams.is_empty() {
        return NO_STREAMS_IN_THIS_RUNTIME_LINE.to_owned();
    }
    let header_row = [
        "ID".to_owned(),
        "NAME".to_owned(),
        "STATE".to_owned(),
        "NODES".to_owned(),
        "PROJECT".to_owned(),
        "ASSET".to_owned(),
    ];
    let stream_rows = listed_streams.iter().map(|listed_stream| {
        let (project_cell, stream_asset_cell) = match &listed_stream.location {
            Some(location) => (
                location.project_directory.display().to_string(),
                location.stream_asset.to_string(),
            ),
            None => (NO_LOCATION_CELL.to_owned(), NO_LOCATION_CELL.to_owned()),
        };
        [
            listed_stream.stream.clone(),
            listed_stream.display_name.clone(),
            listed_stream.state.to_string(),
            listed_stream.node_count.map_or_else(
                || NOT_LOADED_NODE_COUNT_CELL.to_owned(),
                |node_count| node_count.to_string(),
            ),
            project_cell,
            stream_asset_cell,
        ]
    });
    let table_rows: Vec<[String; 6]> = std::iter::once(header_row).chain(stream_rows).collect();
    let mut rendered_table = rendered_padded_table(&table_rows);
    if listed_streams.iter().any(|listed_stream| {
        listed_stream.failed_because.is_some()
            || listed_stream.state == ListedStreamState::Preparing
            || !listed_stream
                .capture_devices_whose_access_was_never_asked_for
                .is_empty()
    }) {
        rendered_table.push('\n');
    }
    for listed_stream in listed_streams
        .iter()
        .filter(|listed_stream| listed_stream.state == ListedStreamState::Preparing)
    {
        let step_cell = listed_stream
            .step
            .map_or(STREAM_ACTION_STILL_QUEUED_CELL, |step| step.wire_spelling());
        rendered_table.push_str(&format!("{} {step_cell}\n", listed_stream.stream));
    }
    for listed_stream in listed_streams {
        let Some(failed_because) = &listed_stream.failed_because else {
            continue;
        };
        let stream_id = &listed_stream.stream;
        let failed_line = match &listed_stream.location {
            Some(location) => format!(
                "{stream_id} failed: {failed_because}. `tatolab start {stream_id}` retries it, \
                 `tatolab rm --dir {}` forgets its project.\n",
                location.project_directory.display()
            ),
            None => format!("{stream_id} failed: {failed_because}.\n"),
        };
        rendered_table.push_str(&failed_line);
    }
    for listed_stream in listed_streams {
        if listed_stream
            .capture_devices_whose_access_was_never_asked_for
            .is_empty()
        {
            continue;
        }
        let capture_devices = listed_stream
            .capture_devices_whose_access_was_never_asked_for
            .iter()
            .map(|capture_device| capture_device.wire_spelling())
            .collect::<Vec<_>>()
            .join(" and ");
        let stream_id = &listed_stream.stream;
        rendered_table.push_str(&format!(
            "{stream_id} is waiting for {capture_devices} access, which nobody has asked for \
             yet: `tatolab stop {stream_id}` then `tatolab start {stream_id}` asks for it.\n"
        ));
    }
    rendered_table
}

/// `set_stream_clock_scale`'s arguments for `stream` at `scale`.
fn set_stream_clock_scale_tool_arguments(
    stream: &str,
    scale: f64,
) -> serde_json::Map<String, serde_json::Value> {
    let mut set_stream_clock_scale_arguments = stream_tool_arguments(stream);
    set_stream_clock_scale_arguments.insert("scale".to_owned(), scale.into());
    set_stream_clock_scale_arguments
}

/// `tatolab clock [STREAM] --scale SCALE`: run the stream's scaled clock at `scale` from now on.
pub(crate) fn set_stream_clock_scale(
    stream_argument: Option<&str>,
    scale: f64,
) -> Result<u8, TatolabCommandFailure> {
    let stream = stream_id_the_stream_argument_names_on_the_running_runtime(stream_argument)?;
    let rescaled: SetStreamClockScaleToolResult = call_one_stream_action_tool(
        StreamActionTool::SetStreamClockScale,
        set_stream_clock_scale_tool_arguments(&stream, scale),
    )?;
    write_verb_standard_output(&rendered_stream_clock_scale_line(&rescaled))
}

fn rendered_stream_clock_scale_line(rescaled: &SetStreamClockScaleToolResult) -> String {
    format!(
        "{} runs its clock at {}× from now on.\n",
        rescaled.stream, rescaled.scale
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    #[test]
    fn run_asks_for_the_canonical_project_and_refuses_a_dir_that_is_not_one() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let project_directory = scratch_directory.path().join("project");
        std::fs::create_dir(&project_directory).unwrap();

        let project_load_request = ProjectLoadRequest::of_the_project_a_verb_names(
            scratch_directory.path(),
            Some(Path::new("./project/.")),
        )
        .unwrap();
        assert_eq!(
            serde_json::Value::Object(project_load_request.run_project_tool_arguments()),
            json!({
                "project_directory": project_directory.canonicalize().unwrap().to_str().unwrap()
            })
        );
        assert_eq!(
            TatolabCommandFailure::refusal_message_of(
                ProjectLoadRequest::of_the_project_a_verb_names(
                    scratch_directory.path(),
                    Some(Path::new("absent")),
                )
            ),
            "--dir absent is not a directory"
        );
    }

    #[test]
    fn the_run_lines_name_the_queued_load_and_how_to_follow_it() {
        let run_project_result = RunProjectToolResult {
            action: "Rabc-1".to_owned(),
            project_directory: PathBuf::from("/srv/project"),
        };

        assert_eq!(
            rendered_run_project_lines(&run_project_result, Path::new("/srv/project")),
            "Rabc-1 queued: load of /srv/project\n  follow it: tatolab logs -f\n"
        );
        assert_eq!(
            rendered_run_project_lines(&run_project_result, Path::new("/srv")),
            "Rabc-1 queued: load of /srv/project\n  follow it: tatolab logs -f --dir /srv/project\n"
        );
    }

    #[test]
    fn start_stop_and_rm_say_which_action_they_queued() {
        assert_eq!(
            rendered_queued_stream_action_line("R-7", "stop", "camera"),
            "R-7 queued: stop of camera\n"
        );
        assert_eq!(
            rendered_queued_stream_action_line("R-9", "remove", "/srv/project"),
            "R-9 queued: remove of /srv/project\n"
        );
    }

    #[test]
    fn the_streams_table_lists_each_stream_by_id_with_its_name_and_location() {
        let list_streams_result: ListStreamsToolResult = serde_json::from_value(json!({
            "streams": [
                {"stream": "cam-main-0a1b2c", "display_name": "Desk camera", "state": "running",
                 "location": {"project_directory": "/srv/cam", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21"},
                 "node_count": 4, "capture_devices_whose_access_was_never_asked_for": []},
                {"stream": "a-back-9f8e7d", "display_name": "back", "state": "stopped",
                 "location": {"project_directory": "/srv/a", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "0f6d2c1e-7a3b-4c5d-9e8f-1a2b3c4d5e6f"},
                 "node_count": null, "capture_devices_whose_access_was_never_asked_for": []},
            ]
        }))
        .unwrap();

        assert_eq!(
            rendered_streams_table(&list_streams_result.streams),
            "ID               NAME         STATE    NODES  PROJECT   ASSET\n\
             cam-main-0a1b2c  Desk camera  running  4      /srv/cam  e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21\n\
             a-back-9f8e7d    back         stopped  -      /srv/a    0f6d2c1e-7a3b-4c5d-9e8f-1a2b3c4d5e6f\n"
        );
        assert_eq!(rendered_streams_table(&[]), "No streams in this runtime.\n");
    }

    #[test]
    fn the_streams_table_says_under_it_why_each_failed_stream_failed() {
        let list_streams_result: ListStreamsToolResult = serde_json::from_value(json!({
            "streams": [
                {"stream": "cam-main-0a1b2c", "display_name": "main", "state": "running",
                 "location": {"project_directory": "/srv/cam", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21"},
                 "node_count": 4, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null},
                {"stream": "main", "display_name": "main", "state": "failed", "location": null,
                 "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": "an earlier runtime recorded it"},
            ]
        }))
        .unwrap();

        assert_eq!(
            rendered_streams_table(&list_streams_result.streams),
            "ID               NAME  STATE    NODES  PROJECT   ASSET\n\
             cam-main-0a1b2c  main  running  4      /srv/cam  e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21\n\
             main             main  failed   -      -         -\n\
             \n\
             main failed: an earlier runtime recorded it.\n"
        );
    }

    #[test]
    fn the_streams_table_says_under_it_which_access_each_waiting_stream_needs_asked_for() {
        let list_streams_result: ListStreamsToolResult = serde_json::from_value(json!({
            "streams": [
                {"stream": "desk-0a1b2c", "display_name": "desk", "state": "running",
                 "location": {"project_directory": "/srv/desk", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21"},
                 "node_count": 3,
                 "capture_devices_whose_access_was_never_asked_for": ["camera", "microphone"]},
            ]
        }))
        .unwrap();

        assert_eq!(
            rendered_streams_table(&list_streams_result.streams),
            "ID           NAME  STATE    NODES  PROJECT    ASSET\n\
             desk-0a1b2c  desk  running  3      /srv/desk  e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21\n\
             \n\
             desk-0a1b2c is waiting for camera and microphone access, which nobody has asked \
             for yet: `tatolab stop desk-0a1b2c` then `tatolab start desk-0a1b2c` asks for it.\n"
        );
    }

    #[test]
    fn the_streams_table_says_under_it_the_step_each_preparing_stream_is_on() {
        let list_streams_result: ListStreamsToolResult = serde_json::from_value(json!({
            "streams": [
                {"stream": "desk-0a1b2c", "display_name": "desk", "state": "preparing",
                 "location": {"project_directory": "/srv/desk", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21"},
                 "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "action": "Rabc-1", "step": "making_the_venv"},
                {"stream": "hall-3d4e5f", "display_name": "hall", "state": "preparing",
                 "location": {"project_directory": "/srv/hall", "project": "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10", "stream_asset": "0f6d2c1e-7a3b-4c5d-9e8f-1a2b3c4d5e6f"},
                 "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "action": "Rabc-2", "step": null},
            ]
        }))
        .unwrap();

        assert_eq!(
            rendered_streams_table(&list_streams_result.streams),
            "ID           NAME  STATE      NODES  PROJECT    ASSET\n\
             desk-0a1b2c  desk  preparing  -      /srv/desk  e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21\n\
             hall-3d4e5f  hall  preparing  -      /srv/hall  0f6d2c1e-7a3b-4c5d-9e8f-1a2b3c4d5e6f\n\
             \n\
             desk-0a1b2c making_the_venv\n\
             hall-3d4e5f queued\n"
        );
    }

    #[test]
    fn an_answer_that_is_not_the_tools_result_is_refused_naming_the_tool() {
        let refusal = tool_result_from::<StopProjectToolResult>(
            StreamActionTool::StopProject.tool_name(),
            "{}",
        )
        .unwrap_err();

        assert!(
            refusal.starts_with("stop_project answered something other than its result ("),
            "{refusal}"
        );
    }
}
