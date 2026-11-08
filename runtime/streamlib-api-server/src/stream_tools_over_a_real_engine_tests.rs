// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream tools over a real engine: a stream run over a `/mcp/stdio`
//! connection outlives the connection and the local API, `logs` pages a
//! stream's records by sequence number, and `list_streams`,
//! `stop_stream`, `start_stream`, `remove_project` and `rename_stream` act by
//! stream id on an engine recording its streams in a temporary state
//! directory, and `open_viewer` opens only over `/mcp/stdio`.
//!
//! A stream's start creates the engine's GPU context, so the GPU-free tests
//! load without starting — through the engine's own loads, or a runtime whose
//! `run_project` loads an empty stream — and the tests that run a project's
//! graph document end to end are gated on `hardware-tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::RoleClient;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use serde_json::{Value, json};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, OptionsForLoadingOneStream, RunProjectRequest,
    Runner, RuntimeOperations, StreamActionQueued, StreamEnvironment, StreamListing,
    StreamLocation, StreamRecord, StreamRecordAsRead, StreamRecordsInTheStateDirectory,
    StreamRenameOutcome,
};
use uuid::Uuid;

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{first_text_block_json, tool_call_result};
use crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream;

/// A native source with one output port, `video`, and nothing to do.
#[streamlib::sdk::processor(execution = manual, output("video", family = Image))]
pub struct StreamToolsTestSource;

impl ManualProcessor for StreamToolsTestSource::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_stream_tools_test_source_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| PROCESSOR_REGISTRY.register::<StreamToolsTestSource::Processor>());
}

/// The graph of a stream named `stream_name`: one [`StreamToolsTestSource`] named `source`.
fn the_graph_of_a_stream_named(stream_name: &str) -> Value {
    json!({
        "stream": stream_name,
        "nodes": [{
            "name": "source",
            "type": StreamToolsTestSource::processor_class_import_path().as_str(),
            "config": {},
        }],
        "links": [],
    })
}

/// Write `script` to `path` executable from a child process, so no thread of
/// this one holds the file open for writing when another execs it.
fn write_an_executable_script_from_a_child_process(path: &Path, script: &str) {
    use std::io::Write;
    let mut script_writer = std::process::Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("a shell writes the script");
    script_writer
        .stdin
        .take()
        .expect("the shell's standard input is piped")
        .write_all(script.as_bytes())
        .expect("the script reaches the shell");
    assert!(script_writer.wait().expect("the shell exits").success());
}

/// A venv interpreter that answers the runtime's probe and runs nothing else.
const STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE: &str = "#!/bin/sh\nexit 0\n";

/// The name every test project's `tatolab-project.json` gives it.
const TEST_PROJECT_NAME: &str = "rig";

/// A project of its own UUID whose streams are the graph documents under `streams/`, and
/// whose `.venv/bin/python` answers the runtime's probe.
struct ProjectWithGraphDocuments {
    _project_directory_removed_on_drop: tempfile::TempDir,
    /// The project directory with its symlinks resolved, as the runtime records
    /// it: macOS's temporary directory is a symlink.
    resolved_project_directory: PathBuf,
    project: Uuid,
}

impl ProjectWithGraphDocuments {
    /// A project with a venv holding one graph document, `graph`'s, named after its `stream`.
    fn holding(graph: Value) -> Self {
        let project = Self::with_no_venv();
        std::fs::create_dir_all(project.interpreter().parent().expect("the venv's bin"))
            .expect("the venv's bin directory");
        write_an_executable_script_from_a_child_process(
            &project.interpreter(),
            STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
        );
        project.write_the_graph_document(graph);
        project
    }

    /// A project with no venv and no graph document.
    fn with_no_venv() -> Self {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let resolved_project_directory = std::fs::canonicalize(project_directory.path())
            .expect("the project directory resolves");
        let project = Self {
            _project_directory_removed_on_drop: project_directory,
            resolved_project_directory,
            project: Uuid::new_v4(),
        };
        std::fs::write(
            project.path().join("pyproject.toml"),
            "[project]\nname = \"project\"\n",
        )
        .expect("the project's pyproject.toml");
        std::fs::write(
            project.path().join("tatolab-project.json"),
            json!({ "project": project.project, "name": TEST_PROJECT_NAME, "dependencies": [] })
                .to_string(),
        )
        .expect("the project's tatolab-project.json");
        project
    }

    /// Write `graph` as the graph document of the stream its `stream` names, holding that
    /// stream's asset UUID.
    fn write_the_graph_document(&self, graph: Value) {
        let stream_name = graph["stream"]
            .as_str()
            .expect("the graph names its stream")
            .to_string();
        let mut document = graph;
        document["asset"] = json!(self.stream_asset_of(&stream_name));
        self.write_the_graph_document_text(&stream_name, &document.to_string());
    }

    /// Write the graph document of the stream `stream_name` holding its asset UUID and a
    /// `nodes` that is no list, which does not load as a graph.
    fn write_an_unloadable_graph_document(&self, stream_name: &str) {
        self.write_the_graph_document_text(
            stream_name,
            &json!({ "asset": self.stream_asset_of(stream_name), "nodes": "not a list" })
                .to_string(),
        );
    }

    fn write_the_graph_document_text(&self, stream_name: &str, document: &str) {
        let streams_directory = self.path().join("streams");
        std::fs::create_dir_all(&streams_directory).expect("the streams directory");
        std::fs::write(
            streams_directory.join(format!("{stream_name}.graph.json")),
            document,
        )
        .expect("the graph document");
    }

    /// Remove the graph document of the stream `stream_name`.
    fn remove_the_graph_document(&self, stream_name: &str) {
        std::fs::remove_file(
            self.path()
                .join("streams")
                .join(format!("{stream_name}.graph.json")),
        )
        .expect("the graph document is removed");
    }

    fn path(&self) -> &Path {
        &self.resolved_project_directory
    }

    fn interpreter(&self) -> PathBuf {
        self.path().join(".venv").join("bin").join("python")
    }

    /// The stream asset UUID of this project's stream `stream_name`.
    fn stream_asset_of(&self, stream_name: &str) -> Uuid {
        Uuid::new_v5(&self.project, stream_name.as_bytes())
    }

    /// Where this project's stream `stream_name` lives.
    fn location_of(&self, stream_name: &str) -> StreamLocation {
        StreamLocation {
            project_directory: self.path().to_path_buf(),
            project: self.project,
            stream_asset: self.stream_asset_of(stream_name),
        }
    }

    /// The location of this project's stream `stream_name` as the local API states it.
    fn location_on_the_wire_of(&self, stream_name: &str) -> Value {
        json!({
            "project_directory": self.path(),
            "project": self.project,
            "stream_asset": self.stream_asset_of(stream_name),
        })
    }

    /// The stream id a first run records this project's stream `stream_name` under.
    fn stream_id_of(&self, stream_name: &str) -> String {
        self.location_of(stream_name)
            .stream_id_named(TEST_PROJECT_NAME, stream_name)
    }

    fn stream_environment(&self) -> StreamEnvironment {
        StreamEnvironment {
            project_directory: self.path().to_path_buf(),
            interpreter: self.interpreter(),
        }
    }
}

/// An engine handed a lend directory, keeping its streams in a temporary
/// state directory.
struct AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    engine: Arc<Runner>,
    streams_directory: PathBuf,
    _state_directory: tempfile::TempDir,
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    fn new() -> Self {
        register_the_stream_tools_test_source_once();
        let engine = Runner::new().expect("the engine builds");
        engine
            .set_processor_interpreter_lend_directory(PathBuf::from(
                "/opt/tatolab/lib/tatolab/lend",
            ))
            .expect("the lend directory is handed over");
        let state_directory = tempfile::tempdir().expect("a state directory");
        let streams_directory = state_directory.path().join("streams");
        engine
            .record_streams_in_the_state_directory(&streams_directory)
            .expect("the engine records its streams there");
        Self {
            engine,
            streams_directory,
            _state_directory: state_directory,
        }
    }

    fn operations_on_the_loaded_streams(
        &self,
    ) -> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
        Arc::clone(&self.engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>
    }

    fn stream_records(&self) -> StreamRecordsInTheStateDirectory {
        StreamRecordsInTheStateDirectory::open(&self.streams_directory)
            .expect("the streams directory opens")
    }

    /// The record of `stream_id` this runtime wrote.
    fn current_record_of(&self, stream_id: &str) -> Option<StreamRecord> {
        match self.stream_records().read(stream_id).unwrap() {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(earlier) => panic!("`{stream_id}` is a record of an earlier runtime: {earlier:?}"),
            None => None,
        }
    }

    /// Load `project`'s stream `stream_name` as its library host would, not started, under
    /// the id its location computes to, which it returns.
    fn a_stream_its_host_loaded_without_its_start(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> String {
        let stream_id = project.stream_id_of(stream_name);
        self.engine
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(stream_name))
                    .expect("the graph parses"),
                OptionsForLoadingOneStream::in_stream_environment(project.stream_environment())
                    .with_display_name(stream_name)
                    .at_stream_location(stream_id.clone(), project.location_of(stream_name)),
            )
            .expect("the host loads its stream");
        stream_id
    }

    /// Record `project`'s stream `stream_name` with its graph document, stopped when
    /// `stopped` says so, and not loaded; returns its id.
    fn a_stream_recorded_and_not_loaded(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
        stopped: bool,
    ) -> String {
        project.write_the_graph_document(the_graph_of_a_stream_named(stream_name));
        let mut record = StreamRecord::of_a_running_stream(
            project.stream_id_of(stream_name),
            project.location_of(stream_name),
            stream_name,
            &project.stream_environment(),
        );
        record.stopped = stopped;
        self.stream_records()
            .write(&record)
            .expect("the record is written");
        record.stream_id
    }
}

/// The JSON a tool's successful result states, or the text of its refusal.
fn tool_answer(tool_result: &Value) -> std::result::Result<Value, String> {
    if tool_result["isError"] == true {
        return Err(tool_result["content"][0]["text"]
            .as_str()
            .expect("a refusal's text block")
            .to_string());
    }
    Ok(first_text_block_json(tool_result))
}

async fn tool_call_over_the_connection(
    client: &RunningService<RoleClient, ()>,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let request = CallToolRequestParams::new(tool_name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default());
    serde_json::to_value(
        client
            .call_tool(request)
            .await
            .unwrap_or_else(|failure| panic!("`{tool_name}` failed below the protocol: {failure}")),
    )
    .unwrap()
}

// ============================================================================
// A stream outlives the connection that ran it
// ============================================================================

/// The action the runtime [`AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt`]
/// answers every run with.
const ACTION_ID_THE_EMPTY_STREAM_RUN_ANSWERS: &str = "R-empty-1";

/// A runtime whose `run_project` loads an empty stream under the project
/// folder's name, standing in for an id, without reading or starting it — a
/// GPU-free stand-in for the engine's own run — answering
/// [`ACTION_ID_THE_EMPTY_STREAM_RUN_ANSWERS`], and hands every other call to the engine.
struct AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt {
    engine: Arc<Runner>,
    project_directory: tempfile::TempDir,
}

impl AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt {
    fn new() -> Self {
        Self {
            engine: Runner::new().expect("the engine builds"),
            project_directory: tempfile::tempdir().expect("a project directory"),
        }
    }
}

impl OperationsOnTheStreamsLoadedInThisRuntime
    for AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt
{
    fn this_machines_name(&self) -> &streamlib::sdk::runtime::MachineName {
        self.engine.this_machines_name()
    }
    fn describe_this_runtime(
        &self,
    ) -> streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult {
        self.engine.describe_this_runtime()
    }
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.engine
            .runtime_operations_of_the_stream_a_call_names(stream_id)
    }
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.engine
            .node_catalog_of_the_stream_a_call_names(stream_id)
    }
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.engine
            .node_types_described_in_the_interpreter_of_the_stream_a_call_names(stream_id)
    }
    fn node_types_installed_modules_declare_in_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<
        Vec<(
            ProcessorDescriptor,
            streamlib::sdk::processors::InstalledModuleOfANodeType,
        )>,
    > {
        self.engine
            .node_types_installed_modules_declare_in_the_stream_a_call_names(stream_id)
    }
    fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine.ids_of_the_loaded_streams()
    }
    fn run_project(&self, request: RunProjectRequest) -> Result<StreamActionQueued> {
        let stream_id = request
            .project_directory
            .file_name()
            .expect("the test names a project folder")
            .to_string_lossy()
            .into_owned();
        self.engine.load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(self.project_directory.path())
                .with_stream_id(&stream_id),
        )?;
        Ok(StreamActionQueued {
            action_id: ACTION_ID_THE_EMPTY_STREAM_RUN_ANSWERS.to_string(),
            project_directory: request.project_directory,
        })
    }
    fn create_project(
        &self,
        request: streamlib::sdk::runtime::CreateProjectRequest,
    ) -> Result<streamlib::sdk::runtime::ProjectCreationQueued> {
        self.engine.create_project(request)
    }
    fn describe_stream_action(
        &self,
        action_id: &str,
    ) -> Result<streamlib::sdk::runtime::StreamAction> {
        self.engine.describe_stream_action(action_id)
    }
    fn cancel_stream_action(
        &self,
        action_id: &str,
    ) -> Result<streamlib::sdk::runtime::StreamAction> {
        self.engine.cancel_stream_action(action_id)
    }
    fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>> {
        self.engine
            .forget_the_stream_of_a_deleted_stream_asset(project, stream_asset)
    }
    fn stop_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::stop_stream(self.engine.as_ref(), stream_id)
    }
    fn start_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::start_stream(self.engine.as_ref(), stream_id)
    }
    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::remove_project(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn rename_stream(&self, stream_id: &str, display_name: &str) -> Result<StreamRenameOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::rename_stream(
            self.engine.as_ref(),
            stream_id,
            display_name,
        )
    }
    fn list_streams(&self) -> Vec<StreamListing> {
        OperationsOnTheStreamsLoadedInThisRuntime::list_streams(self.engine.as_ref())
    }
    fn set_stream_clock_scale(
        &self,
        stream_id: &str,
        scale: f64,
    ) -> Result<streamlib::sdk::context::StreamClockReading> {
        OperationsOnTheStreamsLoadedInThisRuntime::set_stream_clock_scale(
            self.engine.as_ref(),
            stream_id,
            scale,
        )
    }
}

/// The one stream a finished stream action took in.
fn the_one_stream_of(action: &Value) -> &Value {
    match action["streams"].as_array().map(Vec::as_slice) {
        Some([stream]) => stream,
        _ => panic!("the action took in one stream: {action}"),
    }
}

/// A run of the project in the folder `project_folder_name`, which
/// [`AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt`] loads as the stream
/// of that id.
fn a_run_request_of_the_project_in(project_folder_name: &str) -> Value {
    json!({ "project_directory": format!("/home/someone/projects/{project_folder_name}") })
}

/// A stream run over a `/mcp/stdio` connection outlives it: dropping the
/// connection — a killed CLI, a closed terminal, an MCP host quitting —
/// unloads nothing, and a new connection lists the stream running.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_run_over_an_mcp_stdio_connection_outlives_it_and_lists_running() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let run = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_project",
            a_run_request_of_the_project_in("camera"),
        )
        .await,
    )
    .expect("the run is answered");
    assert_eq!(
        run,
        json!({
            "action": ACTION_ID_THE_EMPTY_STREAM_RUN_ANSWERS,
            "project_directory": "/home/someone/projects/camera",
        })
    );

    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(engine.ids_of_the_loaded_streams(), ["camera"]);
    let next_session = rmcp_client_over_the_upgraded_stream(&served).await;
    let listed =
        tool_answer(&tool_call_over_the_connection(&next_session, "list_streams", json!({})).await)
            .expect("list_streams answers");
    assert_eq!(listed["streams"][0]["stream"], "camera", "{listed}");
    assert_eq!(listed["streams"][0]["state"], "running", "{listed}");
}

/// Stopping the local API ends every upgraded connection and unloads no
/// stream run over one.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_run_over_an_mcp_stdio_connection_outlives_the_local_api_stopping() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let mut served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_project",
            a_run_request_of_the_project_in("camera"),
        )
        .await,
    )
    .expect("the run is answered");

    served.stop_serving();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(engine.ids_of_the_loaded_streams(), ["camera"]);
    drop(client);
}

// ============================================================================
// logs
// ============================================================================

/// `logs` reads a project's log from disk: `stream` narrows it to one of the
/// project's streams, `count` bounds a page, `next_after` reads on, and the
/// records stay readable once the stream is unloaded.
#[tokio::test(flavor = "multi_thread")]
async fn logs_reads_a_projects_records_by_stream_paged_by_its_cursor() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let engine = Runner::new().expect("the engine builds");
    for stream_id in ["paged", "other"] {
        let stream = engine
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .with_stream_id(stream_id),
            )
            .expect("the stream loads");
        stream.log_route().run_entered(|| {
            for token_index in 0..3 {
                tracing::info!("logs-paging-token-{stream_id}-{token_index}");
            }
        });
        stream.log_route().write_every_record_it_emitted_and_sync();
    }
    let operations_on_the_loaded_streams =
        Arc::clone(&engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let project = project_directory.path().to_str().unwrap().to_owned();
    let tokens = |page: &Value| -> Vec<String> {
        page["records"]
            .as_array()
            .expect("a records array")
            .iter()
            .filter_map(|record| record["message"].as_str())
            .filter(|message| message.starts_with("logs-paging-token-"))
            .map(str::to_owned)
            .collect()
    };
    let read = |arguments: Value| {
        let operations = Arc::clone(&operations_on_the_loaded_streams);
        async move {
            tool_answer(&tool_call_result(operations, "logs", arguments).await)
                .expect("logs answers")
        }
    };

    let of_paged = read(json!({ "project_directory": project, "stream": "paged" })).await;
    assert_eq!(
        tokens(&of_paged),
        [
            "logs-paging-token-paged-0",
            "logs-paging-token-paged-1",
            "logs-paging-token-paged-2"
        ]
    );
    assert_eq!(of_paged["project_directory"], project.as_str());
    assert_eq!(of_paged["records_no_longer_held"], false);
    let every_record = read(json!({ "project_directory": project })).await;
    assert_eq!(tokens(&every_record).len(), 6, "{every_record}");

    let mut cursor = Value::Null;
    let mut read_one_by_one = Vec::new();
    loop {
        let page = read(json!({
            "project_directory": project,
            "stream": "paged",
            "after": cursor,
            "count": 1,
        }))
        .await;
        let records = page["records"].as_array().unwrap();
        if records.is_empty() {
            break;
        }
        assert_eq!(records.len(), 1, "{page}");
        read_one_by_one.extend(tokens(&page));
        cursor = page["next_after"].clone();
    }
    assert_eq!(read_one_by_one, tokens(&of_paged));

    engine.unload_stream("paged").expect("the stream unloads");
    engine.unload_stream("other").expect("the stream unloads");
    let after_the_unload = read(json!({ "project_directory": project, "stream": "paged" })).await;
    assert_eq!(tokens(&after_the_unload), tokens(&of_paged));

    let refusal = tool_answer(
        &tool_call_result(
            operations_on_the_loaded_streams,
            "logs",
            json!({ "project_directory": "relative/project" }),
        )
        .await,
    )
    .expect_err("a relative project directory is refused");
    assert!(refusal.contains("absolute path"), "{refusal}");
}

// ============================================================================
// list, stop, start and remove against an engine recording its streams
// ============================================================================

/// `list_streams` lists each stream by its id with its display name and
/// location, and every tool resolves a stream by that id exactly.
#[tokio::test(flavor = "multi_thread")]
async fn list_streams_lists_each_stream_by_id_and_every_tool_resolves_that_id_exactly() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let waiting = engine.a_stream_recorded_and_not_loaded(&project, "waiting", false);

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");

    let location_of = |stream_name: &str| project.location_on_the_wire_of(stream_name);
    let mut expected_streams = vec![
        json!({ "stream": live, "display_name": "live", "location": location_of("live"), "state": "running", "node_count": 1, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
        json!({ "stream": parked, "display_name": "parked", "location": location_of("parked"), "state": "stopped", "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
        json!({ "stream": waiting, "display_name": "waiting", "location": location_of("waiting"), "state": "running", "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
    ];
    expected_streams.sort_by_key(|listed| listed["stream"].as_str().unwrap().to_string());
    assert_eq!(listed, json!({ "streams": expected_streams }));

    let graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": live }),
        )
        .await,
    );
    assert_eq!(graph["stream"], live.as_str(), "{graph}");
    for not_the_id in ["live", live.to_uppercase().as_str()] {
        let refusal = tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "graph",
                json!({ "stream": not_the_id }),
            )
            .await,
        )
        .expect_err("a stream is named by its exact id");
        assert!(
            refusal.contains(not_the_id) && refusal.contains(&live),
            "{refusal}"
        );
    }
}

/// `open_viewer` is refused over `POST /mcp`, whose connection ends with its
/// answer, and over `/mcp/stdio` names a node that is not running; the
/// connection closes only a viewer it opened.
#[tokio::test(flavor = "multi_thread")]
async fn open_viewer_is_refused_over_post_and_for_a_node_not_running() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": live }),
        )
        .await,
    );
    let source_id = graph["nodes"]
        .as_array()
        .expect("the graph lists its nodes")
        .iter()
        .find(|node| node["name"] == "source")
        .and_then(|node| node["id"].as_str())
        .expect("the graph lists the source node's id")
        .to_string();
    let the_ports_viewer = json!({ "stream": live, "node_id": source_id, "port": "video" });

    let over_post = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "open_viewer",
            the_ports_viewer.clone(),
        )
        .await,
    )
    .expect_err("a `POST /mcp` request holds no viewer open");
    assert!(over_post.contains("/mcp/stdio"), "{over_post}");

    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    let not_running =
        tool_answer(&tool_call_over_the_connection(&client, "open_viewer", the_ports_viewer).await)
            .expect_err("a node that is not running has nothing to view");
    assert!(
        not_running.contains("source") && not_running.contains("not running"),
        "{not_running}"
    );
    let unknown = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "close_viewer",
            json!({ "viewer": format!("{live}/{source_id}/video") }),
        )
        .await,
    )
    .expect_err("the connection opened no viewer");
    assert!(unknown.contains("no viewer"), "{unknown}");
}

/// `set_stream_clock_scale` reaches the named stream's clock, refusing a stream
/// whose clock is not running and a stream the runtime has not loaded.
#[tokio::test(flavor = "multi_thread")]
async fn set_stream_clock_scale_refuses_a_stream_with_no_clock_running() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");

    let not_started = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "set_stream_clock_scale",
            json!({ "stream": live, "scale": 0.5 }),
        )
        .await,
    )
    .expect_err("a stream that is not started has no clock to scale");
    assert!(
        not_started.contains(&format!("`{live}` is not started")),
        "{not_started}"
    );

    let not_loaded = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "set_stream_clock_scale",
            json!({ "stream": "nowhere", "scale": 0.5 }),
        )
        .await,
    )
    .expect_err("a stream that is not loaded is refused");
    assert!(
        not_loaded.contains("nowhere") && not_loaded.contains(&live),
        "{not_loaded}"
    );
}

/// Stop, start and remove through the tools: each queues its action and
/// answers it at once, refuses what it cannot do naming why with nothing
/// queued, and its action leaves the state directory as it says.
#[tokio::test(flavor = "multi_thread")]
async fn stop_start_and_remove_project_queue_actions_on_the_streams_the_runtime_holds() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let refusal = call("start_stream", json!({ "stream": live }))
        .await
        .expect_err("a loaded stream does not start");
    assert!(
        refusal.contains("already loaded") && refusal.contains("living with its host process"),
        "{refusal}"
    );
    let refusal = call("start_stream", json!({ "stream": "nothing" }))
        .await
        .expect_err("an id no record holds does not start");
    assert!(
        refusal.contains("nothing") && refusal.contains(&parked),
        "{refusal}"
    );
    let refusal = call("stop_stream", json!({ "stream": parked }))
        .await
        .expect_err("a stopped stream does not stop again");
    assert!(refusal.contains("already stopped"), "{refusal}");

    let stop = call("stop_stream", json!({ "stream": live }))
        .await
        .unwrap();
    assert_eq!(stop["stream"], live.as_str(), "{stop}");
    let stopped = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        stop["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(stopped["kind"], "stop", "{stopped}");
    assert_eq!(stopped["state"], "done", "{stopped}");
    assert_eq!(stopped["streams"][0]["state"], "stopped", "{stopped}");
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());

    let removal = call(
        "remove_project",
        json!({ "project_directory": project.path() }),
    )
    .await
    .unwrap();
    let removed = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        removal["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(removed["kind"], "remove", "{removed}");
    assert_eq!(removed["state"], "done", "{removed}");
    assert_eq!(engine.current_record_of(&parked), None);
    let refusal = call(
        "remove_project",
        json!({ "project_directory": project.path() }),
    )
    .await
    .expect_err("a project the runtime no longer holds a stream of is refused");
    assert!(
        refusal.contains("holds no stream of the project") && refusal.contains("Loaded: none"),
        "{refusal}"
    );
}

/// `rename_stream` shows a stream under a new display name by its id alone:
/// a recorded stream's record carries it, a loaded one's listing shows it,
/// and the id the call named is the id it answers.
#[tokio::test(flavor = "multi_thread")]
async fn rename_stream_shows_a_stream_under_a_new_display_name_and_records_a_recorded_one() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    assert_eq!(
        call(
            "rename_stream",
            json!({ "stream": parked, "display_name": "Hello camera" })
        )
        .await
        .unwrap(),
        json!({ "stream": parked, "display_name": "Hello camera" })
    );
    assert_eq!(
        call(
            "rename_stream",
            json!({ "stream": live, "display_name": "Desk mic" })
        )
        .await
        .unwrap(),
        json!({ "stream": live, "display_name": "Desk mic" })
    );

    assert_eq!(
        engine
            .current_record_of(&parked)
            .expect("the record is there")
            .display_name,
        "Hello camera"
    );
    let listed = call("list_streams", json!({})).await.unwrap();
    let shown_as: Vec<(&str, &str)> = listed["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listed| {
            (
                listed["stream"].as_str().unwrap(),
                listed["display_name"].as_str().unwrap(),
            )
        })
        .collect();
    let mut expected = vec![
        (live.as_str(), "Desk mic"),
        (parked.as_str(), "Hello camera"),
    ];
    expected.sort_unstable();
    assert_eq!(shown_as, expected);
    let refusal = call(
        "rename_stream",
        json!({ "stream": "Hello camera", "display_name": "Other" }),
    )
    .await
    .expect_err("a display name is never an id");
    assert!(
        refusal.contains("`Hello camera`") && refusal.contains(&parked),
        "{refusal}"
    );
}

/// A record an earlier runtime wrote is listed failed, keyed by its file
/// name, with why and how to run its stream again, until `remove_project` of
/// the project it names forgets it.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_an_earlier_runtime_wrote_is_listed_failed_until_it_is_removed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    std::fs::write(
        engine.streams_directory.join("main.json"),
        json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": "/home/someone/hello",
            "interpreter": "/home/someone/hello/.venv/bin/python",
            "stream_function": null,
            "graph": the_graph_of_a_stream_named("main"),
            "stopped": false,
            "failed_because": null,
            "runtime_crashes_in_a_row_implicating_it": 0,
            "exposure_rulings": [],
        })
        .to_string(),
    )
    .expect("the earlier runtime's record is written");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let listed = call("list_streams", json!({})).await.unwrap();
    let earlier = &listed["streams"][0];
    assert_eq!(earlier["stream"], "main", "{listed}");
    assert_eq!(earlier["state"], "failed", "{listed}");
    assert_eq!(earlier["location"], Value::Null, "{listed}");
    let failed_because = earlier["failed_because"].as_str().unwrap();
    assert!(
        failed_because.contains("an earlier runtime recorded it")
            && failed_because.contains("/home/someone/hello"),
        "{failed_because}"
    );
    let refusal = call("start_stream", json!({ "stream": "main" }))
        .await
        .expect_err("a record an earlier runtime wrote does not start");
    assert!(
        refusal.contains("an earlier runtime recorded it"),
        "{refusal}"
    );

    let removal = call(
        "remove_project",
        json!({ "project_directory": "/home/someone/hello" }),
    )
    .await
    .unwrap();
    let removed = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        removal["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(removed["state"], "done", "{removed}");
    assert_eq!(
        call("list_streams", json!({})).await.unwrap(),
        json!({ "streams": [] })
    );
}

/// On a runtime handed no uv, a recorded stream whose interpreter is gone does
/// not start and is recorded failed saying the runtime unit carries no uv; a run
/// of a project with no venv records the project failed, and its action is done.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_or_run_needing_a_venv_on_a_runtime_given_no_uv_records_it_failed_saying_so() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("gone"));
    let gone = engine.a_stream_recorded_and_not_loaded(&project, "gone", true);
    std::fs::remove_file(project.interpreter()).expect("the interpreter goes");

    let start = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "start_stream",
            json!({ "stream": gone }),
        )
        .await,
    )
    .expect("the start is queued");
    let started = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        start["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(started["state"], "done", "{started}");
    assert_eq!(
        started["streams"][0]["state"], "failed",
        "a recorded stream whose venv cannot be made does not start: {started}"
    );
    let Some(StreamRecordAsRead::Current(gone_record)) = engine
        .stream_records()
        .read(&gone)
        .expect("the record reads")
    else {
        panic!("the record of `{gone}` stays, as this runtime wrote it");
    };
    let gone_failed_because = gone_record.failed_because.expect("the stream is failed");
    assert!(
        gone_failed_because.contains("the runtime unit carries no uv"),
        "{gone_failed_because}"
    );

    let project_with_no_venv = ProjectWithGraphDocuments::with_no_venv();
    project_with_no_venv.write_the_graph_document(the_graph_of_a_stream_named("main"));
    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project_with_no_venv.path(),
    )
    .await;
    assert_eq!(run["state"], "done", "{run}");
    let project_failed = the_one_stream_of(&run);
    assert_eq!(project_failed["state"], "failed", "{run}");
    let listing = the_listing_of(
        &engine.operations_on_the_loaded_streams(),
        project_failed["stream"].as_str().unwrap(),
    )
    .await;
    assert!(
        listing["failed_because"]
            .as_str()
            .is_some_and(|failed_because| failed_because.contains("the runtime unit carries no uv")),
        "{listing}"
    );
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
}

/// A run whose graph document does not load finishes done with the stream failed, listed
/// failed naming the document under the id its location computes to.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_graph_document_does_not_load_records_the_stream_failed_naming_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_an_unloadable_graph_document("camera");
    let camera_stream_id = project.stream_id_of("camera");

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(
        run["streams"],
        json!([{
            "stream": camera_stream_id,
            "display_name": "camera",
            "state": "failed",
        }])
    );
    let listing = the_listing_of(
        &engine.operations_on_the_loaded_streams(),
        &camera_stream_id,
    )
    .await;
    assert_eq!(listing["state"], "failed");
    assert!(
        listing["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("`streams/camera.graph.json` does not load")),
        "{listing}"
    );
    assert_eq!(
        listing["location"],
        project.location_on_the_wire_of("camera")
    );
}

/// A run of a folder holding no `tatolab-project.json` fails its action saying so, and
/// records no stream.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_of_a_folder_that_is_no_project_fails_its_action_saying_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let no_project = tempfile::tempdir().expect("a folder");

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        no_project.path(),
    )
    .await;

    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("is not a project: it holds no tatolab-project.json")),
        "{run}"
    );
    assert_eq!(run["streams"], json!([]), "{run}");
    assert_eq!(
        tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "list_streams",
                json!({}),
            )
            .await,
        )
        .unwrap(),
        json!({ "streams": [] })
    );
}

/// `describe_stream_action`'s answer for the run of `project_directory` that
/// `run_project` queued, once it has finished.
async fn the_run_of_the_project_once_finished(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "run_project",
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    the_stream_action_once_finished(
        operations_on_the_loaded_streams,
        queued["action"].as_str().expect("the action's id"),
    )
    .await
}

/// [`the_run_of_the_project_once_finished`], the run queued over `connection`.
async fn the_run_over_the_connection_once_finished(
    connection: &RunningService<RoleClient, ()>,
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    let queued = tool_answer(
        &tool_call_over_the_connection(
            connection,
            "run_project",
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    the_stream_action_once_finished(
        operations_on_the_loaded_streams,
        queued["action"].as_str().expect("the action's id"),
    )
    .await
}

/// The listing `list_streams` gives of the stream `stream_id`.
async fn the_listing_of(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    stream_id: &str,
) -> Value {
    let listed = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");
    listed["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|listing| listing["stream"] == stream_id)
        .unwrap_or_else(|| panic!("`{stream_id}` is listed: {listed}"))
        .clone()
}

/// `describe_stream_action`'s answer for `action` once its `state` is `done`, `failed` or
/// `cancelled`.
async fn the_stream_action_once_finished(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    action: &str,
) -> Value {
    the_stream_action_once(operations_on_the_loaded_streams, action, |described| {
        ["done", "failed", "cancelled"].contains(&described["state"].as_str().unwrap_or_default())
    })
    .await
}

/// `describe_stream_action`'s answer for `action` once `reached` holds of it.
async fn the_stream_action_once(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    action: &str,
    reached: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let described = tool_answer(
            &tool_call_result(
                Arc::clone(operations_on_the_loaded_streams),
                "describe_stream_action",
                json!({ "action": action }),
            )
            .await,
        )
        .expect("the action is held");
        if reached(&described) {
            return described;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the action never got there: {described}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A project directory holding only a venv, so `create_project` writes every file of a
/// new project.
fn a_directory_for_create_project() -> ProjectWithGraphDocuments {
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::remove_dir_all(project.path().join("streams")).unwrap();
    std::fs::remove_file(project.path().join("tatolab-project.json")).unwrap();
    std::fs::remove_file(project.path().join("pyproject.toml")).unwrap();
    project
}

/// `create_project` named a directory: what it answers, and the action once finished.
async fn create_project_in(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> (Value, Value) {
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "create_project",
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("a create of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;
    (queued, finished)
}

/// `create_project` in a named directory writes a new project's files and loads its
/// `streams/main.graph.json` under the folder's name, answering the stream's id before it
/// runs.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_in_a_directory_writes_the_project_and_runs_its_stream_under_the_folders_name()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_directory_for_create_project();
    let folder_name = project
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let (queued, finished) =
        create_project_in(operations_on_the_loaded_streams, project.path()).await;

    let stream_id = queued["stream"]
        .as_str()
        .expect("the stream's id")
        .to_string();
    assert_eq!(
        queued,
        json!({
            "action": finished["action"],
            "stream": stream_id,
            "project_directory": project.path(),
        })
    );
    assert_eq!(finished["state"], "done", "{finished}");
    let [created_stream] = finished["streams"].as_array().unwrap().as_slice() else {
        panic!("the create took in one stream: {finished}");
    };
    assert_eq!(
        (&created_stream["stream"], &created_stream["display_name"]),
        (&json!(stream_id), &json!(folder_name)),
        "{finished}"
    );
    for file_a_new_project_starts_with in [
        "tatolab-project.json",
        "streams/main.graph.json",
        "pyproject.toml",
        ".python-version",
        ".gitignore",
        "nodes/__init__.py",
    ] {
        assert!(
            project
                .path()
                .join(file_a_new_project_starts_with)
                .is_file(),
            "{file_a_new_project_starts_with}"
        );
    }
    assert_eq!(
        engine
            .current_record_of(&stream_id)
            .expect("the stream is recorded")
            .display_name,
        folder_name
    );
}

/// `create_project` over a directory already holding a file a new project starts with
/// fails its action with nothing written.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_over_a_file_a_new_project_starts_with_fails_its_action_with_nothing_written()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_directory_for_create_project();
    std::fs::create_dir_all(project.path().join("streams")).unwrap();
    std::fs::write(
        project.path().join("streams/main.graph.json"),
        "# the user's own\n",
    )
    .unwrap();

    let (_queued, finished) =
        create_project_in(operations_on_the_loaded_streams, project.path()).await;

    assert_eq!(finished["state"], "failed", "{finished}");
    assert!(
        finished["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("already has streams/main.graph.json")),
        "{finished}"
    );
    assert!(!project.path().join("pyproject.toml").exists());
    assert_eq!(
        std::fs::read_to_string(project.path().join("streams/main.graph.json")).unwrap(),
        "# the user's own\n"
    );
}

/// `create_project` with a relative directory is refused, nothing created.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_with_a_relative_directory_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "create_project",
            json!({ "project_directory": "a-relative-project" }),
        )
        .await,
    )
    .expect_err("a relative directory is refused");
    assert!(refusal.contains("must be an absolute path"), "{refusal}");
    assert!(!Path::new("a-relative-project").exists());
}

/// `run_project` answers its action at once, and the action is followed to `done` with the
/// stream of the graph document that does not load failed.
#[tokio::test(flavor = "multi_thread")]
async fn run_project_queues_a_run_followed_by_describe_stream_action() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_an_unloadable_graph_document("camera");
    let project_directory = project.path().to_path_buf();

    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "run_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    assert_eq!(queued["project_directory"], json!(project_directory));

    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    assert_eq!(
        finished,
        json!({
            "action": action,
            "project_directory": project_directory,
            "kind": "load",
            "state": "done",
            "step": null,
            "streams": [{
                "stream": project.stream_id_of("camera"),
                "display_name": "camera",
                "state": "failed",
            }],
            "failed_because": null,
        })
    );
}

/// A run cancelled while the runtime's uv makes its venv answers `in_progress`, has uv
/// killed and ends `cancelled` with its stream never loaded; a second cancel is refused,
/// the action finished.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_stream_action_stops_a_running_venv_sync_and_the_action_ends_cancelled() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_that_never_finishes = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_that_never_finishes,
        "#!/bin/sh
exec sleep 30
",
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_that_never_finishes)
        .expect("the runtime is handed its uv");
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "run_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    the_stream_action_once(operations_on_the_loaded_streams, &action, |described| {
        described["step"] == "making_the_venv"
    })
    .await;
    let asked_at = std::time::Instant::now();

    let cancel = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "cancel_stream_action",
            json!({ "action": action }),
        )
        .await,
    )
    .expect("a running load takes its cancel");
    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    assert_eq!(cancel, json!({ "action": action, "state": "in_progress" }));
    assert_eq!(
        (
            &finished["state"],
            &finished["step"],
            &finished["streams"],
            &finished["failed_because"]
        ),
        (
            &json!("cancelled"),
            &Value::Null,
            &json!([{
                "stream": project.stream_id_of("camera"),
                "display_name": "camera",
                "state": null,
            }]),
            &Value::Null
        ),
        "{finished}"
    );
    assert!(
        asked_at.elapsed() < Duration::from_secs(10),
        "the sync was waited for, not killed"
    );
    let again = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "cancel_stream_action",
            json!({ "action": action }),
        )
        .await,
    )
    .unwrap_err();
    assert!(again.contains("is already cancelled"), "{again}");
}

/// An action the runtime never queued, and a relative project directory, are refused
/// saying so.
#[tokio::test(flavor = "multi_thread")]
async fn describe_stream_action_and_run_project_refuse_what_the_runtime_cannot_hold() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();

    let never_queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "describe_stream_action",
            json!({ "action": "Rgone-7" }),
        )
        .await,
    )
    .unwrap_err();
    assert!(
        never_queued.contains("the stream action `Rgone-7` is not held"),
        "{never_queued}"
    );
    let relative = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "run_project",
            json!({ "project_directory": "desk" }),
        )
        .await,
    )
    .unwrap_err();
    assert!(
        relative.contains("`project_directory` must be an absolute path"),
        "{relative}"
    );
}

// ============================================================================
// End to end on the engine's own run — the start creates its GPU context
// ============================================================================

/// A project run over a `/mcp/stdio` connection is recorded and runs on once
/// the connection drops, and stop, start and remove act on it through the
/// tools.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_project_end_to_end_outlives_its_connection_through_stop_start_and_remove() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera_stream_id = project.stream_id_of("camera");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let run = the_run_over_the_connection_once_finished(
        &client,
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(
        run["streams"],
        json!([{
            "stream": camera_stream_id,
            "display_name": "camera",
            "state": "running",
        }])
    );
    assert!(
        engine.current_record_of(&camera_stream_id).is_some(),
        "the run is recorded"
    );

    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        std::slice::from_ref(&camera_stream_id)
    );

    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    for (tool_name, arguments, state_the_action_leaves) in [
        (
            "stop_stream",
            json!({ "stream": camera_stream_id }),
            json!("stopped"),
        ),
        (
            "start_stream",
            json!({ "stream": camera_stream_id }),
            json!("running"),
        ),
        (
            "remove_project",
            json!({ "project_directory": project.path() }),
            Value::Null,
        ),
    ] {
        let queued = call(tool_name, arguments).await.unwrap();
        let finished = the_stream_action_once_finished(
            &operations_on_the_loaded_streams,
            queued["action"].as_str().unwrap(),
        )
        .await;
        assert_eq!(finished["state"], "done", "{tool_name}: {finished}");
        if !state_the_action_leaves.is_null() {
            assert_eq!(
                finished["streams"][0]["state"], state_the_action_leaves,
                "{tool_name}: {finished}"
            );
        }
    }
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
}

/// `tatolab dev`'s reload: a run of a project replaces its recorded stream under
/// the same id and another load, whichever connection carries it; a bad save
/// records the stream failed and unloads it; a graph document of another stream
/// loads it under its own id, and a run once a document is gone forgets its stream;
/// and closing every connection unloads nothing.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_of_a_recorded_location_replaces_its_stream_from_any_connection() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let first_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let other_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let camera_stream_id = project.stream_id_of("camera");
    let stream_tag_loaded_as_camera = || {
        engine
            .engine
            .loaded_stream_of_the_id(&camera_stream_id)
            .unwrap()
            .stream_tag()
    };

    the_run_over_the_connection_once_finished(
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let first_load = stream_tag_loaded_as_camera();

    project.write_an_unloadable_graph_document("camera");
    let bad_save = the_run_over_the_connection_once_finished(
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    assert_eq!(
        the_one_stream_of(&bad_save)["state"],
        "failed",
        "{bad_save}"
    );
    assert!(
        the_listing_of(&operations_on_the_loaded_streams, &camera_stream_id).await["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("does not load")),
    );
    assert!(
        engine.engine.ids_of_the_loaded_streams().is_empty(),
        "a bad save unloads the running stream"
    );

    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let replaced = the_run_over_the_connection_once_finished(
        &other_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let replaced = the_one_stream_of(&replaced);
    assert_eq!(replaced["stream"], camera_stream_id.as_str());
    assert_eq!(replaced["state"], "running");
    assert_ne!(stream_tag_loaded_as_camera(), first_load);

    project.remove_the_graph_document("camera");
    project.write_the_graph_document(the_graph_of_a_stream_named("studio"));
    the_run_over_the_connection_once_finished(
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let studio_stream_id = project.stream_id_of("studio");
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        std::slice::from_ref(&studio_stream_id)
    );
    let listed_ids: Vec<String> = engine
        .engine
        .list_streams()
        .into_iter()
        .map(|listing| listing.stream_id)
        .collect();
    assert_eq!(listed_ids, std::slice::from_ref(&studio_stream_id));

    drop(other_connection);
    drop(first_connection);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        [studio_stream_id]
    );
}
