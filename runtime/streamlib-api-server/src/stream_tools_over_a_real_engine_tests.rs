// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream tools over a real engine: a stream run over a `/mcp/stdio`
//! connection outlives the connection and the local API, `logs` pages a
//! stream's records by sequence number, and `list_streams`,
//! `stop_playback`, `remove_project` and the stream asset tools act on
//! an engine recording its streams in a temporary state
//! directory, `open_viewer` opens only over `/mcp/stdio`, and graph edits are
//! transactions kept in a stream's graph document that each author's `undo` reverses.
//!
//! A stream's start creates the engine's GPU context, so the GPU-free tests
//! load without starting — through the engine's own loads, or a runtime whose
//! `open_project` loads an empty stream — and the tests that run a project's
//! graph document end to end are gated on `hardware-tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use rmcp::RoleClient;
use rmcp::service::RunningService;
use serde_json::{Value, json};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, OptionsForLoadingOneStream,
    ProjectOpenOrReloadRequest, Runner, RuntimeOperations, StreamActionQueued, StreamEnvironment,
    StreamListing, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordsInTheStateDirectory,
};
use streamlib_project_repository::{GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE};
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
                OptionsForLoadingOneStream::at_stream_location(
                    stream_id.clone(),
                    project.location_of(stream_name),
                )
                .in_stream_environment(project.stream_environment())
                .with_display_name(stream_name),
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
    let request = crate::mcp::tests::the_tool_call_that_reaches(tool_name, &arguments);
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

/// A runtime whose `open_project` and `reload_project` load an empty stream under the project
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
    fn loaded_stream_identity_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<streamlib::sdk::pubsub::LoadedStreamIdentity> {
        self.engine
            .loaded_stream_identity_of_the_stream_a_call_names(stream_id)
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
    fn run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
        generator: &str,
        fragment_name: Option<&str>,
        promoted_settings: &serde_json::Map<String, Value>,
    ) -> Result<Value> {
        self.engine
            .run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
                stream_id,
                generator,
                fragment_name,
                promoted_settings,
            )
    }
    fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine.ids_of_the_loaded_streams()
    }
    fn open_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        let stream_id = request
            .project_directory
            .file_name()
            .expect("the test names a project folder")
            .to_string_lossy()
            .into_owned();
        self.engine
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                self.project_directory.path(),
                &stream_id,
            ))?;
        Ok(StreamActionQueued {
            action_id: ACTION_ID_THE_EMPTY_STREAM_RUN_ANSWERS.to_string(),
            project_directory: request.project_directory,
        })
    }
    fn reload_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        self.open_project(request)
    }
    fn close_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::close_project(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn start_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::start_playback(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn create_project(
        &self,
        request: streamlib::sdk::runtime::CreateProjectRequest,
    ) -> Result<streamlib::sdk::runtime::ProjectCreationQueued> {
        self.engine.create_project(request)
    }
    fn list_samples(
        &self,
    ) -> streamlib_runtime_client_contract::local_api_wire_contract::ListSamplesToolResult {
        self.engine.list_samples()
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
    fn stream_actions_under_way(&self) -> Vec<streamlib::sdk::runtime::StreamAction> {
        self.engine.stream_actions_under_way()
    }
    fn listen_for_each_stream_action_change(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.engine.listen_for_each_stream_action_change()
    }
    fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>> {
        self.engine
            .forget_the_stream_of_a_deleted_stream_asset(project, stream_asset)
    }
    fn stop_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::stop_playback(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn pause_playback(
        &self,
        project_directory: &Path,
    ) -> Result<streamlib::sdk::runtime::ProjectPausedOrResumed> {
        OperationsOnTheStreamsLoadedInThisRuntime::pause_playback(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn resume_playback(
        &self,
        project_directory: &Path,
    ) -> Result<streamlib::sdk::runtime::ProjectPausedOrResumed> {
        OperationsOnTheStreamsLoadedInThisRuntime::resume_playback(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::remove_project(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn delete_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::delete_project(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn show_the_stream_of_a_stream_asset_under(
        &self,
        project: uuid::Uuid,
        stream_asset: uuid::Uuid,
        display_name: &str,
    ) -> Option<String> {
        OperationsOnTheStreamsLoadedInThisRuntime::show_the_stream_of_a_stream_asset_under(
            self.engine.as_ref(),
            project,
            stream_asset,
            display_name,
        )
    }
    fn load_the_stream_of_a_created_stream_asset(
        &self,
        project_directory: &Path,
        stream_asset: uuid::Uuid,
    ) -> Option<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::load_the_stream_of_a_created_stream_asset(
            self.engine.as_ref(),
            project_directory,
            stream_asset,
        )
    }
    fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
            self.engine.as_ref(),
            project_directory,
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
    fn lock_the_python_requirements_of_the_project_in(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String> {
        OperationsOnTheStreamsLoadedInThisRuntime::lock_the_python_requirements_of_the_project_in(
            self.engine.as_ref(),
            project_directory,
        )
    }
    fn record_first_party_package_moves_in(
        &self,
        history: Arc<dyn streamlib::sdk::runtime::ProjectHistoryRecordingFirstPartyPackageMoves>,
    ) {
        OperationsOnTheStreamsLoadedInThisRuntime::record_first_party_package_moves_in(
            self.engine.as_ref(),
            history,
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
            "open_project",
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
            "open_project",
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

/// A subscriber to a project's log, through a symlink to it, is told `project_log_records_written`
/// naming the project as it spelled it once a record it emits is written, and `logs` then reads
/// that record; a project it did not subscribe to is never named.
///
/// Mental-revert: without the drain worker's announcement the wait below times out, without the
/// connection's filter the other project's write is sent, and naming the project with symlinks
/// resolved sends a directory the subscriber never named.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_log_subscriber_is_told_each_write_and_reads_the_new_record_with_logs() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let another_project_directory = tempfile::tempdir().expect("another project directory");
    let engine = Runner::new().expect("the engine builds");
    let stream = engine
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "logged",
        ))
        .expect("the stream loads");
    let stream_of_another_project = engine
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            another_project_directory.path(),
            "elsewhere",
        ))
        .expect("the other stream loads");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    )
    .await;
    let symlinks_directory = tempfile::tempdir().expect("a directory for a symlink");
    let project_directory_through_a_symlink = symlinks_directory.path().join("logged-project");
    std::os::unix::fs::symlink(
        project_directory.path(),
        &project_directory_through_a_symlink,
    )
    .expect("the symlink to the project is made");
    let project_directory_as_subscribed = project_directory_through_a_symlink.display().to_string();
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_log_records",
                      "params": { "project_directories": [project_directory_as_subscribed] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"],
        json!({ "project_directories": [project_directory_as_subscribed] })
    );

    stream_of_another_project.log_route().run_entered(|| {
        tracing::info!("project-log-push-token-of-another-project");
    });
    stream_of_another_project
        .log_route()
        .write_every_record_it_emitted_and_sync();
    stream.log_route().run_entered(|| {
        tracing::info!("project-log-push-token");
    });
    stream.log_route().write_every_record_it_emitted_and_sync();

    let mut project_log_writes_told = Vec::new();
    while let Ok(written) = tokio::time::timeout(
        Duration::from_secs(1),
        the_next_notification_under(&mut subscriber, "project_log_records_written"),
    )
    .await
    {
        project_log_writes_told.push(written);
    }
    assert!(
        project_log_writes_told.contains(
            &json!({ "project_directory": project_directory_as_subscribed, "change": "written" })
        ),
        "{project_log_writes_told:?}"
    );
    assert!(
        project_log_writes_told
            .iter()
            .all(|written| written["project_directory"] == project_directory_as_subscribed.as_str()),
        "{project_log_writes_told:?}"
    );
    let page = served
        .control_call(
            "logs",
            json!({ "project_directory": project_directory_as_subscribed }),
        )
        .await;
    let messages: Vec<&str> = page["result"]["records"]
        .as_array()
        .expect("a records array")
        .iter()
        .filter_map(|record| record["message"].as_str())
        .collect();
    assert!(messages.contains(&"project-log-push-token"), "{page}");
}

/// `logs` reads a project's log from disk: `stream` narrows it to one of the
/// project's streams, `count` bounds a page, `next_after` reads on, and the
/// records stay readable once the stream is unloaded.
#[tokio::test(flavor = "multi_thread")]
async fn logs_reads_a_projects_records_by_stream_paged_by_its_cursor() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let engine = Runner::new().expect("the engine builds");
    for stream_id in ["paged", "other"] {
        let stream = engine
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                stream_id,
            ))
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
// list, stop and remove against an engine recording its streams
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
    let not_running_result =
        tool_call_over_the_connection(&client, "open_viewer", the_ports_viewer.clone()).await;
    let not_running = tool_answer(&not_running_result)
        .expect_err("a node that is not running has nothing to view");
    assert!(
        not_running.contains("source") && not_running.contains("not running"),
        "{not_running}"
    );
    let refusal = &not_running_result["structuredContent"]["refusal"];
    assert_eq!(refusal["action"], "open_viewer", "{refusal}");
    assert_eq!(refusal["not_running_because"], "preparing", "{refusal}");
    assert_eq!(
        format!(
            "open_viewer failed: {}",
            refusal["runtime_words"].as_str().unwrap()
        ),
        not_running
    );

    engine
        .engine
        .loaded_stream_of_the_id(&live)
        .expect("the stream is loaded")
        .hold_stopped_without_a_start();
    let stopped_result =
        tool_call_over_the_connection(&client, "open_viewer", the_ports_viewer).await;
    assert_eq!(
        stopped_result["structuredContent"]["refusal"]["not_running_because"], "playback_stopped",
        "{stopped_result}"
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

/// Stop and remove a project through the tools: each queues its action and
/// answers it at once, refuses what it cannot do naming why with nothing
/// queued, and its action leaves the state directory as it says.
#[tokio::test(flavor = "multi_thread")]
async fn stop_playback_and_remove_project_queue_actions_on_the_streams_the_runtime_holds() {
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

    let refusal = call(
        "stop_playback",
        json!({ "project_directory": "relative/project" }),
    )
    .await
    .expect_err("a relative project directory does not stop");
    assert!(refusal.contains("absolute"), "{refusal}");

    let stop = call(
        "stop_playback",
        json!({ "project_directory": project.path() }),
    )
    .await
    .unwrap();
    assert_eq!(
        stop["project_directory"],
        std::fs::canonicalize(project.path())
            .unwrap()
            .display()
            .to_string(),
        "{stop}"
    );
    let stopped = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        stop["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(stopped["kind"], "stop", "{stopped}");
    assert_eq!(stopped["state"], "done", "{stopped}");
    assert_eq!(
        stopped["streams"].as_array().unwrap().len(),
        1,
        "the stopped `parked` is left out of the stop: {stopped}"
    );
    assert_eq!(stopped["streams"][0]["stream"], live.as_str(), "{stopped}");
    assert_eq!(stopped["streams"][0]["state"], "stopped", "{stopped}");
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
    let refusal = call(
        "stop_playback",
        json!({ "project_directory": project.path() }),
    )
    .await
    .expect_err("a project none of whose streams runs does not stop again");
    assert!(refusal.contains("none to stop"), "{refusal}");

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

/// `set_stream_display_name` writes a stream's display name in its graph document: a loaded
/// stream is shown under it at once and keeps its id, and a stream recorded and not loaded is
/// listed under it; `delete_stream` forgets a recorded stream, and `create_stream` in a
/// project with a loaded stream queues the load of the stream it created.
#[tokio::test(flavor = "multi_thread")]
async fn the_stream_tools_name_delete_and_create_a_projects_streams_in_their_graph_documents() {
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
    let shown_as = |listed: &Value| -> Vec<(String, String)> {
        listed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|listed| {
                (
                    listed["stream"].as_str().unwrap().to_owned(),
                    listed["display_name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };

    for (stream_name, display_name) in [("parked", "Hello camera"), ("live", "Desk mic")] {
        let named = call(
            "set_stream_display_name",
            json!({
                "project_directory": project.path(),
                "stream_asset": project.stream_asset_of(stream_name),
                "display_name": display_name,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            named["asset"]["asset"],
            json!(project.stream_asset_of(stream_name))
        );
        assert_eq!(named["asset"]["kind"], "stream");
    }
    let mut expected = vec![
        (live.clone(), "Desk mic".to_owned()),
        (parked.clone(), "Hello camera".to_owned()),
    ];
    expected.sort_unstable();
    assert_eq!(
        shown_as(&call("list_streams", json!({})).await.unwrap()),
        expected
    );

    call(
        "delete_stream",
        json!({
            "project_directory": project.path(),
            "stream_asset": project.stream_asset_of("parked"),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        shown_as(&call("list_streams", json!({})).await.unwrap()),
        vec![(live, "Desk mic".to_owned())]
    );
    assert!(engine.current_record_of(&parked).is_none());

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/preview.graph.json",
            "display_name": "Preview",
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["asset"]["path"], "streams/preview.graph.json");
    let load_of_the_created_stream = call(
        "describe_stream_action",
        json!({ "action": created["action"] }),
    )
    .await
    .unwrap();
    assert_eq!(
        load_of_the_created_stream["project_directory"],
        json!(project.path()),
        "a running project loads the stream it created: {load_of_the_created_stream}"
    );
}

/// An undo of `set_stream_display_name` shows the loaded stream under its former name again,
/// and an undo of `create_stream` unloads and forgets the stream the creation loaded.
#[tokio::test(flavor = "multi_thread")]
async fn undoing_a_stream_tool_brings_the_loaded_streams_back_in_line() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    let listed_stream_ids_and_names = || async {
        let listed = call("list_streams", json!({})).await.unwrap();
        listed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|listed| {
                (
                    listed["stream"].as_str().unwrap().to_owned(),
                    listed["display_name"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let undo = || call("undo", json!({ "project_directory": project.path() }));

    call(
        "set_stream_display_name",
        json!({
            "project_directory": project.path(),
            "stream_asset": project.stream_asset_of("live"),
            "display_name": "Desk mic",
        }),
    )
    .await
    .unwrap();
    undo().await.expect("the display name change undoes");
    assert_eq!(
        listed_stream_ids_and_names().await,
        vec![(live.clone(), "live".to_owned())]
    );

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/preview.graph.json",
            "display_name": "Preview",
        }),
    )
    .await
    .unwrap();
    the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        created["action"]
            .as_str()
            .expect("the creation queues a load"),
    )
    .await;
    assert_eq!(
        listed_stream_ids_and_names().await.len(),
        2,
        "the created stream is listed once its load ran"
    );
    undo().await.expect("the creation undoes");
    assert_eq!(
        listed_stream_ids_and_names().await,
        vec![(live, "live".to_owned())]
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

/// A record an earlier runtime wrote, under the id a stream of `project_directory` computes to.
fn write_a_record_an_earlier_runtime_wrote(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    project_directory: &Path,
) {
    std::fs::write(
        engine.streams_directory.join(format!("{stream_id}.json")),
        json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": project_directory,
            "graph": the_graph_of_a_stream_named("main"),
            "stopped": false,
            "failed_because": null,
        })
        .to_string(),
    )
    .expect("the earlier runtime's record is written");
}

/// A run of a project whose stream id is held by a record an earlier runtime wrote from
/// that same project directory runs, as the record's `failed_because` says it will, and
/// its record replaces the earlier one.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_of_a_project_an_earlier_runtime_recorded_replaces_that_record() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let main_stream_id = project.stream_id_of("main");
    write_a_record_an_earlier_runtime_wrote(&engine, &main_stream_id, project.path());

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "done", "{run}");
    let ran = the_one_stream_of(&run);
    assert_eq!(ran["stream"], main_stream_id.as_str(), "{run}");
    let listing = the_listing_of(&engine.operations_on_the_loaded_streams(), &main_stream_id).await;
    assert_ne!(ran["state"], "failed", "{run} {listing}");
    assert_eq!(listing["failed_because"], Value::Null, "{listing}");
    assert_eq!(
        listing["location"],
        project.location_on_the_wire_of("main"),
        "{listing}"
    );
}

/// A record an earlier runtime wrote from another directory still holds its id against a
/// run, naming that directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_an_earlier_runtime_wrote_from_another_directory_still_holds_its_id() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let main_stream_id = project.stream_id_of("main");
    write_a_record_an_earlier_runtime_wrote(
        &engine,
        &main_stream_id,
        Path::new("/home/someone/elsewhere"),
    );

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["failed_because"]
            .as_str()
            .is_some_and(|why| why
                .contains("a record an earlier runtime wrote from /home/someone/elsewhere")),
        "{run}"
    );
}

/// On a runtime handed no uv, a run of a project with no venv records the project failed
/// saying the runtime unit carries no uv, and its action is done.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_needing_a_venv_on_a_runtime_given_no_uv_records_it_failed_saying_so() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
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

/// `describe_stream_action`'s answer for the open of `project_directory` that
/// `open_project` queued, once it has finished.
async fn the_run_of_the_project_once_finished(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    the_project_action_once_finished(
        "open_project",
        operations_on_the_loaded_streams,
        project_directory,
    )
    .await
}

/// `describe_stream_action`'s answer for the action the tool `tool_name` queued on
/// `project_directory`, once it has finished.
async fn the_project_action_once_finished(
    tool_name: &str,
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            tool_name,
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("an action on a project is queued");
    the_stream_action_once_finished(
        operations_on_the_loaded_streams,
        queued["action"].as_str().expect("the action's id"),
    )
    .await
}

/// [`the_run_of_the_project_once_finished`], the start or reload `start_or_reload_tool_name`
/// names queued over `connection`.
async fn the_run_over_the_connection_once_finished(
    start_or_reload_tool_name: &str,
    connection: &RunningService<RoleClient, ()>,
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    let queued = tool_answer(
        &tool_call_over_the_connection(
            connection,
            start_or_reload_tool_name,
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
    ] {
        assert!(
            project
                .path()
                .join(file_a_new_project_starts_with)
                .is_file(),
            "{file_a_new_project_starts_with}"
        );
    }
    assert!(engine.current_record_of(&stream_id).is_some());
    let main_graph_document: Value = serde_json::from_slice(
        &std::fs::read(project.path().join("streams/main.graph.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(main_graph_document["display_name"], json!(folder_name));
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

/// `create_project` naming a sample the runtime unit does not carry is refused at once, naming
/// the samples it does, and `list_samples` lists none for a unit that carries none.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_from_a_sample_the_unit_does_not_carry_is_refused_naming_what_it_carries() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_directory_for_create_project();

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "create_project",
            json!({ "project_directory": project.path(), "sample": "camera-halftone" }),
        )
        .await,
    )
    .expect_err("a sample the unit does not carry is refused");

    assert!(
        refusal.contains("the runtime unit carries no sample `camera-halftone`; it carries none"),
        "{refusal}"
    );
    assert!(!project.path().join("tatolab-project.json").exists());
    assert_eq!(
        tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "list_samples",
                json!({}),
            )
            .await,
        )
        .expect("the samples are listed"),
        json!({ "samples": [] })
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

/// `open_project` answers its action at once, and the action is followed to `done` with the
/// stream of the graph document that does not load failed.
#[tokio::test(flavor = "multi_thread")]
async fn open_project_queues_an_open_followed_by_describe_stream_action() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_an_unloadable_graph_document("camera");
    let project_directory = project.path().to_path_buf();

    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
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
            "kind": "open",
            "state": "done",
            "step": null,
            "venv_sync": null,
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
            "open_project",
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

/// A graph document deleted while its project's load makes the venv is left out when the load
/// starts its streams: no record of its stream is written, and the project's other stream is.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_document_deleted_while_its_load_makes_the_venv_never_records_its_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    project.write_the_graph_document(the_graph_of_a_stream_named("screen"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_released = uv_directory.path().join("released");
    let uv_held_until_released = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_held_until_released,
        &format!(
            "#!/bin/sh
while [ ! -e '{}' ]; do sleep 0.05; done
exit 1
",
            uv_released.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_held_until_released)
        .expect("the runtime is handed its uv");
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
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

    std::fs::remove_file(project.path().join("streams/screen.graph.json"))
        .expect("the screen stream's graph document is deleted");
    std::fs::write(&uv_released, "").expect("uv is released");
    the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    let listed = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");
    let listed_stream_ids: Vec<&str> = listed["streams"]
        .as_array()
        .expect("the streams listed")
        .iter()
        .map(|listed_stream| listed_stream["stream"].as_str().expect("a stream id"))
        .collect();
    assert_eq!(
        listed_stream_ids,
        vec![project.stream_id_of("camera").as_str()],
        "{listed}"
    );
}

/// An action the runtime never queued, and a relative project directory, are refused
/// saying so.
#[tokio::test(flavor = "multi_thread")]
async fn describe_stream_action_and_open_project_refuse_what_the_runtime_cannot_hold() {
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
            "open_project",
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

/// A project opened and played over a `/mcp/stdio` connection is recorded and plays on once
/// the connection drops, and `stop_playback`, `start_playback`, `close_project` and
/// `remove_project` act on it through the tools.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_played_project_end_to_end_outlives_its_connection_through_stop_play_close_and_remove() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera_stream_id = project.stream_id_of("camera");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let opened = the_run_over_the_connection_once_finished(
        "open_project",
        &client,
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let run = the_run_over_the_connection_once_finished(
        "start_playback",
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
            "stop_playback",
            json!({ "project_directory": project.path() }),
            json!("stopped"),
        ),
        (
            "start_playback",
            json!({ "project_directory": project.path() }),
            json!("running"),
        ),
        (
            "close_project",
            json!({ "project_directory": project.path() }),
            Value::Null,
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
        "open_project",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    the_run_over_the_connection_once_finished(
        "start_playback",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let first_load = stream_tag_loaded_as_camera();

    project.write_an_unloadable_graph_document("camera");
    let bad_save = the_run_over_the_connection_once_finished(
        "reload_project",
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
        "reload_project",
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
        "reload_project",
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

// ============================================================================
// Graph edits are transactions
// ============================================================================

/// A native sink with one input port, `video`, and nothing to do.
#[streamlib::sdk::processor(
    execution = manual,
    input("video", delivery_profile = "newest", family = Image)
)]
pub struct StreamToolsTestSink;

impl ManualProcessor for StreamToolsTestSink::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_stream_tools_test_sink_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| PROCESSOR_REGISTRY.register::<StreamToolsTestSink::Processor>());
}

/// The UUID the graph document of [`a_project_whose_main_stream_holds_one_source`] gives its
/// one node.
const SOURCE_NODE_UUID: &str = "5a5a5a5a-5a5a-4a5a-8a5a-5a5a5a5a5a5a";

/// A project whose stream `main` holds one [`StreamToolsTestSource`] named `source`, by
/// [`SOURCE_NODE_UUID`].
fn a_project_whose_main_stream_holds_one_source() -> ProjectWithGraphDocuments {
    register_the_stream_tools_test_sink_once();
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"][0]["id"] = json!(SOURCE_NODE_UUID);
    ProjectWithGraphDocuments::holding(graph)
}

impl ProjectWithGraphDocuments {
    /// The graph document of this project's stream `stream_name`, as it is on disk now.
    fn the_graph_document_of(&self, stream_name: &str) -> Value {
        serde_json::from_slice(
            &std::fs::read(
                self.path()
                    .join("streams")
                    .join(format!("{stream_name}.graph.json")),
            )
            .expect("the graph document reads"),
        )
        .expect("the graph document is JSON")
    }
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Load `project`'s stream `stream_name` from its graph document on disk as its library
    /// host would, not started; returns its id.
    fn a_stream_loaded_from_its_graph_document(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> String {
        self.a_stream_loaded_from_the_graph_document_of_the_stream_asset(
            project,
            stream_name,
            project.stream_asset_of(stream_name),
        )
    }

    /// Load `project`'s stream `stream_name`, whose graph document holds the stream asset
    /// `stream_asset`, as its library host would, not started; returns its id.
    fn a_stream_loaded_from_the_graph_document_of_the_stream_asset(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
        stream_asset: Uuid,
    ) -> String {
        let stream_location = StreamLocation {
            stream_asset,
            ..project.location_of(stream_name)
        };
        let stream_id = stream_location.stream_id_named(TEST_PROJECT_NAME, stream_name);
        self.engine
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(
                    streamlib_project_repository::graph_document_with_each_stream_fragment_reference_resolved(
                        project.path(),
                        &project.the_graph_document_of(stream_name),
                    )
                    .expect("each stream fragment of the graph document resolves"),
                )
                .expect("the graph document parses"),
                OptionsForLoadingOneStream::at_stream_location(stream_id.clone(), stream_location)
.in_stream_environment(project.stream_environment())
                    .with_display_name(stream_name),
            )
            .expect("the stream loads");
        stream_id
    }

    /// Why `project`'s stream `stream_name` is refused loading from its graph document.
    fn a_stream_refused_loading_from_its_graph_document(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> streamlib::sdk::error::Error {
        let stream_location = project.location_of(stream_name);
        let stream_id = stream_location.stream_id_named(TEST_PROJECT_NAME, stream_name);
        match self.engine.load_stream_from_graph_snapshot(
            &GraphSnapshot::from_graph_document(project.the_graph_document_of(stream_name))
                .expect("the graph document parses"),
            OptionsForLoadingOneStream::at_stream_location(stream_id, stream_location)
                .in_stream_environment(project.stream_environment())
                .with_display_name(stream_name),
        ) {
            Ok(_) => panic!("the stream `{stream_name}` loads, and a refusal was expected"),
            Err(refusal) => refusal,
        }
    }

    /// The live graph of the loaded stream `stream_id`.
    fn the_live_graph_of(&self, stream_id: &str) -> Value {
        self.engine
            .runtime_operations_of_the_stream_a_call_names(stream_id)
            .expect("the stream is loaded")
            .to_json()
            .expect("the graph renders")
    }
}

/// Each node's name in `graph`, sorted, leaving out a node a stream not started holds
/// pending its deletion at the start.
fn sorted_node_names_in(graph: &Value) -> Vec<String> {
    let mut names: Vec<String> = graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["components"]["pending_deletion"] != true)
        .filter_map(|node| node["name"].as_str().map(str::to_owned))
        .collect();
    names.sort_unstable();
    names
}

/// Each link in `graph` as `source/port -> target/port`, by node name, leaving out a link
/// pending its deletion.
fn links_by_node_name_in(graph: &Value) -> Vec<String> {
    let name_of = |end: &Value| {
        let node = end["node"].as_str().unwrap_or_default();
        graph["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|candidate| candidate["id"] == node)
            .and_then(|candidate| candidate["name"].as_str())
            .unwrap_or(node)
            .to_owned()
    };
    graph["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|link| link["components"]["pending_deletion"] != true)
        .map(|link| {
            format!(
                "{}/{} -> {}/{}",
                name_of(&link["source"]),
                link["source"]["port"].as_str().unwrap_or_default(),
                name_of(&link["target"]),
                link["target"]["port"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

fn an_agent_named(name: &str) -> Value {
    json!({ "kind": "agent", "name": name })
}

/// Five edits sent as one `transact` are one commit: the graph document keeps them, so a
/// fresh runtime loading it finds them, and one `undo` reverses all five, on the document and
/// on the running stream.
/// The graph actions a `transact` is sent, and the one action a single-operation tool is sent,
/// are what their commits record, field for field, once the mode, the author and the key that
/// carry them are set aside.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_action_sent_over_mcp_is_the_action_its_commit_records() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let transacted_actions = json!([
        { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
          "name": "monitor", "as": "monitor" },
        { "operation": "connect", "stream": stream_asset,
          "source": { "node": SOURCE_NODE_UUID, "port": "video" },
          "target": { "node": "monitor", "port": "video" } },
    ]);
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({
                "author": an_agent_named("alice"), "idempotency_key": "wire-monitor",
                "operations": transacted_actions,
            }),
        )
        .await,
    )
    .expect("the transaction applies");
    let rename = json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID, "name": "camera" });
    let mut rename_sent = rename.clone();
    rename_sent["author"] = an_agent_named("alice");
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "rename_node",
            rename_sent,
        )
        .await,
    )
    .expect("the rename applies");

    let recorded_actions_oldest_first: Vec<Value> =
        streamlib_project_repository::ProjectRepository::open(project.path())
            .expect("the project opens")
            .applied_transactions_newest_first(2)
            .expect("history reads")
            .into_iter()
            .rev()
            .map(|applied| serde_json::to_value(&applied.transaction.actions).unwrap())
            .collect();
    let mut rename_as_recorded = rename;
    rename_as_recorded["operation"] = json!("rename_node");
    assert_eq!(
        recorded_actions_oldest_first,
        vec![transacted_actions, json!([rename_as_recorded])]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_control_stream_subscriber_is_sent_a_transact_another_client_made_and_both_doors_agree() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;

    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_changes",
                      "params": { "projects": [project.project] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["projects"],
        json!([project.project])
    );

    let applied = tool_answer(
        &served
            .mcp_tool_result(
                "transact",
                json!({
                    "author": an_agent_named("alice"),
                    "operations": [{ "operation": "rename_node", "stream": stream_asset,
                                     "node": SOURCE_NODE_UUID, "name": "camera" }],
                }),
            )
            .await,
    )
    .expect("the transaction applies");

    let notification = subscriber.next_message_but_files_changed().await;
    assert_eq!(notification["method"], "project_changed", "{notification}");
    assert_eq!(notification["params"]["change"], "applied");
    assert_eq!(
        notification["params"]["transaction"]["transaction"],
        applied["transaction"]
    );
    assert_eq!(
        notification["params"]["transaction"]["author_name"],
        "alice"
    );

    let graph_over_mcp = tool_answer(
        &served
            .mcp_tool_result("graph", json!({ "stream": stream_id }))
            .await,
    )
    .expect("the graph exports");
    let graph_over_control = served
        .control_call("graph", json!({ "stream": stream_id }))
        .await;
    assert_eq!(
        graph_over_control["result"]["nodes"],
        graph_over_mcp["nodes"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_project_changes_over_control_answers_the_change_feed_its_resource_renders() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;
    let applied = tool_answer(
        &served
            .mcp_tool_result(
                "transact",
                json!({
                    "author": an_agent_named("alice"),
                    "operations": [{ "operation": "rename_node", "stream": stream_asset,
                                     "node": SOURCE_NODE_UUID, "name": "camera" }],
                }),
            )
            .await,
    )
    .expect("the transaction applies");

    let read_over_control = served
        .control_call(
            "read_project_changes",
            json!({ "project_directory": project.path() }),
        )
        .await;
    let change_feed = &read_over_control["result"];
    let resource_read = serde_json::to_value(
        served
            .mcp_client
            .read_resource(rmcp::model::ReadResourceRequestParams::new(
                crate::mcp_project_change_feed::project_transactions_resource_uri(project.project),
            ))
            .await
            .expect("the change feed's resource reads"),
    )
    .unwrap();
    let change_feed_over_mcp: Value =
        serde_json::from_str(resource_read["contents"][0]["text"].as_str().unwrap()).unwrap();

    assert_eq!(
        change_feed["project"],
        json!(project.project),
        "{read_over_control}"
    );
    assert_eq!(change_feed["transactions"][0]["status"], "applied");
    assert_eq!(
        change_feed["transactions"][0]["transaction"],
        applied["transaction"]
    );
    assert_eq!(change_feed["transactions"][0]["author_name"], "alice");
    assert_eq!(*change_feed, change_feed_over_mcp);
}

/// The next notification `subscriber` is sent under `method`, skipping any other.
async fn the_next_notification_under(
    subscriber: &mut crate::control_json_rpc_adapter::tests::ControlWebSocketMessages,
    method: &str,
) -> Value {
    loop {
        let message = subscriber.next_message().await;
        if message["method"] == method {
            return message["params"].clone();
        }
    }
}

/// The next message that answers a request, past every notification sent before it.
async fn the_next_answer_but_its_notifications(
    subscriber: &mut crate::control_json_rpc_adapter::tests::ControlWebSocketMessages,
) -> Value {
    loop {
        let message = subscriber.next_message().await;
        if message.get("id").is_some() {
            return message;
        }
    }
}

/// The params of the next `stream_graph_changed` that is not a sample of drops, raises and link
/// states, which a subscriber is sent on each stream's first sample.
async fn the_next_stream_graph_change_but_its_samples(
    subscriber: &mut crate::control_json_rpc_adapter::tests::ControlWebSocketMessages,
) -> Value {
    loop {
        let stream_graph_change =
            the_next_notification_under(subscriber, "stream_graph_changed").await;
        if stream_graph_change["change"] != "drops_raises_or_link_states_changed" {
            return stream_graph_change;
        }
    }
}

/// A subscriber to a loaded stream's graph is sent each edit another client makes, is sent
/// `missed` when the stream is unloaded and again when it is loaded once more, and follows it to
/// its new load; a subscriber to the stream actions is sent each change of the close and the
/// open.
///
/// Mental-revert: a subscription left on the first load's topic hears nothing of the edit after
/// the open, and the last wait below fails.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_graph_subscriber_is_sent_each_edit_and_follows_its_stream_to_its_next_load() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let opened =
        the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path())
            .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let stream_id = project.stream_id_of("main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&operations_on_the_loaded_streams),
    )
    .await;

    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_stream_graph_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"]["streams"],
        json!([stream_id])
    );
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "subscribe_stream_action_changes",
                      "params": {} }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"],
        json!({ "actions_under_way": [] })
    );

    let added = served
        .control_call(
            "add_node",
            json!({ "stream": stream_asset, "type": sink_type.as_str(), "name": "monitor" }),
        )
        .await;
    assert!(added.get("result").is_some(), "{added}");
    assert_eq!(
        the_next_stream_graph_change_but_its_samples(&mut subscriber).await,
        json!({ "stream": stream_id, "change": "edited", "node": null, "node_state": null })
    );

    for (project_action, how_the_stream_graph_is_left) in [
        ("close_project", "unloaded"),
        ("open_project", "loaded again"),
    ] {
        let queued = served
            .control_call(
                project_action,
                json!({ "project_directory": project.path() }),
            )
            .await;
        let action = queued["result"]["action"]
            .as_str()
            .unwrap_or_else(|| panic!("{project_action} is queued: {queued}"))
            .to_owned();
        let finished =
            the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
        assert_eq!(finished["state"], "done", "{project_action}: {finished}");
        loop {
            let stream_action_change =
                the_next_notification_under(&mut subscriber, "stream_action_changed").await;
            assert_eq!(
                stream_action_change["change"], "changed",
                "{stream_action_change}"
            );
            if stream_action_change["action"] == action {
                break;
            }
        }
        loop {
            let stream_graph_change =
                the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
            if stream_graph_change["change"] == "missed" {
                assert_eq!(
                    stream_graph_change["stream"], stream_id,
                    "the stream's graph {how_the_stream_graph_is_left}"
                );
                break;
            }
        }
    }

    let added_on_the_new_load = served
        .control_call(
            "add_node",
            json!({ "stream": stream_asset, "type": sink_type.as_str(), "name": "second-monitor" }),
        )
        .await;
    assert!(
        added_on_the_new_load.get("result").is_some(),
        "{added_on_the_new_load}"
    );
    loop {
        let stream_graph_change =
            the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
        if stream_graph_change["change"] == "edited" {
            assert_eq!(stream_graph_change["stream"], stream_id);
            break;
        }
    }
}

/// A native source whose every `process()` raises the same words.
#[streamlib::sdk::processor(execution = continuous(interval_ms = 5), output("video", family = Image))]
pub struct StreamToolsTestSourceWhoseProcessRaises;

/// What [`StreamToolsTestSourceWhoseProcessRaises`] raises.
const WHAT_THE_RAISING_SOURCE_RAISES: &str = "the test source raises on every process";

impl streamlib::sdk::processors::ContinuousProcessor
    for StreamToolsTestSourceWhoseProcessRaises::Processor
{
    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        Err(streamlib::sdk::error::Error::Runtime(
            WHAT_THE_RAISING_SOURCE_RAISES.to_owned(),
        ))
    }
}

/// A subscriber to a stream's graph is sent `drops_raises_or_link_states_changed` on the stream's
/// first sample, and again once a node's `process()` starts raising after playback starts, which no
/// graph event reports, and the graph it reads then carries the raise.
///
/// Mental-revert: without the sampling, the raise reaches a pushed client only on its next
/// unrelated change, and the wait below times out.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_stream_graph_subscriber_is_sent_a_raise_no_graph_event_reports() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        PROCESSOR_REGISTRY.register::<StreamToolsTestSourceWhoseProcessRaises::Processor>()
    });
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "raising",
        "nodes": [{
            "id": SOURCE_NODE_UUID,
            "name": "source",
            "type": StreamToolsTestSourceWhoseProcessRaises::processor_class_import_path().as_str(),
            "config": {},
        }],
        "links": [],
    }));
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let opened =
        the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path())
            .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let stream_id = project.stream_id_of("raising");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&operations_on_the_loaded_streams),
    )
    .await;
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_stream_graph_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["streams"],
        json!([stream_id])
    );

    loop {
        let first_sample =
            the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
        if first_sample["change"] == "drops_raises_or_link_states_changed" {
            break;
        }
    }

    let queued = served
        .control_call(
            "start_playback",
            json!({ "project_directory": project.path() }),
        )
        .await;
    let action = queued["result"]["action"]
        .as_str()
        .unwrap_or_else(|| panic!("start_playback is queued: {queued}"))
        .to_owned();
    let started = the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
    assert_eq!(started["state"], "done", "{started}");

    let graph_carrying_the_raise = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let stream_graph_change =
                the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
            if stream_graph_change["change"] != "drops_raises_or_link_states_changed" {
                continue;
            }
            assert_eq!(
                stream_graph_change["stream"], stream_id,
                "{stream_graph_change}"
            );
            let graph = served
                .control_call("graph", json!({ "stream": stream_id }))
                .await;
            let raising_because =
                graph["result"]["nodes"][0]["components"]["process_raising_because"]
                    .as_str()
                    .unwrap_or_default();
            if raising_because.contains(WHAT_THE_RAISING_SOURCE_RAISES) {
                return graph;
            }
        }
    })
    .await;
    assert!(
        graph_carrying_the_raise.is_ok(),
        "no drops_raises_or_link_states_changed was followed by a graph carrying the raise"
    );
}

/// `subscribe_stream_graph_changes` is refused on a one-shot `POST /control`, for no stream, and
/// for a stream not loaded, and `subscribe_stream_action_changes` on a one-shot `POST /control`.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_subscription_is_refused_off_the_websocket_and_for_a_stream_not_loaded() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;

    for (method, params) in [
        (
            "subscribe_stream_graph_changes",
            json!({ "streams": [stream_id] }),
        ),
        ("subscribe_stream_action_changes", json!({})),
    ] {
        let refused = served.control_call(method, params).await;
        let refusal = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(refusal.contains("/control/ws"), "{refused}");
    }

    let mut subscriber = served.control_websocket().await;
    for (id, streams, refusal_names) in [
        (1, json!([]), "names no stream".to_owned()),
        (2, json!(["no-such-stream"]), "no-such-stream".to_owned()),
    ] {
        subscriber
            .send(
                json!({ "jsonrpc": "2.0", "id": id, "method": "subscribe_stream_graph_changes",
                          "params": { "streams": streams } }),
            )
            .await;
        let refused = subscriber.next_message().await;
        assert_eq!(refused["id"], id, "{refused}");
        let refusal = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(refusal.contains(&refusal_names), "{refused}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn five_edits_in_one_transaction_are_kept_across_a_restart_and_one_undo_reverses_them() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();

    let applied = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
                      "name": "monitor", "as": "monitor" },
                    { "operation": "connect", "stream": stream_asset,
                      "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                      "target": { "node": "monitor", "port": "video" } },
                    { "operation": "rename_node", "stream": stream_asset,
                      "node": SOURCE_NODE_UUID, "name": "camera" },
                    { "operation": "set_studio_fields", "stream": stream_asset,
                      "node": SOURCE_NODE_UUID, "studio": { "position": [120, 40] } },
                    { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
                      "name": "spare" },
                ],
            }),
        )
        .await,
    )
    .expect("the transaction applies");
    assert_eq!(applied["mode"], "commit", "{applied}");
    assert!(applied["transaction"].is_string(), "{applied}");
    assert!(applied["commit"].is_string(), "{applied}");
    assert_eq!(applied["operations"].as_array().map(Vec::len), Some(5));

    let kept = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&kept),
        ["camera", "monitor", "spare"],
        "{kept}"
    );
    assert_eq!(
        links_by_node_name_in(&kept),
        ["camera/video -> monitor/video"],
        "{kept}"
    );
    let camera = kept["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == SOURCE_NODE_UUID)
        .expect("the source keeps its UUID");
    assert_eq!(
        camera["studio"],
        json!({ "contract": 1, "position": [120, 40] })
    );

    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(
        sorted_node_names_in(&reloaded),
        ["camera", "monitor", "spare"]
    );
    assert_eq!(
        links_by_node_name_in(&reloaded),
        ["camera/video -> monitor/video"]
    );

    let undone = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    assert_eq!(undone["undoes"], applied["transaction"], "{undone}");
    let after_the_undo = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&after_the_undo),
        ["source"],
        "{after_the_undo}"
    );
    assert_eq!(links_by_node_name_in(&after_the_undo), Vec::<String>::new());
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(sorted_node_names_in(&live), ["source"], "{live}");
    assert_eq!(links_by_node_name_in(&live), Vec::<String>::new(), "{live}");
}

/// Two authors' edits interleave, and each author's `undo` reverses only that author's own;
/// `redo` brings back what the author's undo reversed.
#[tokio::test(flavor = "multi_thread")]
async fn each_authors_undo_reverses_only_their_own_edits() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal}"))
        }
    };
    let add_a_sink_as = |author: &str, name: &str| {
        json!({
            "stream": stream_asset, "type": sink_type.as_str(), "name": name,
            "author": an_agent_named(author),
        })
    };
    let undo_by = |author: &str| json!({ "project_directory": project.path(), "author": an_agent_named(author) });

    let alice_first = call("add_node", add_a_sink_as("alice", "alice-first")).await["added_nodes"]
        [0]["node"]
        .clone();
    call("add_node", add_a_sink_as("bob", "bob-first")).await;
    call(
        "rename_node",
        json!({
            "stream": stream_asset, "node": alice_first, "name": "alice-renamed",
            "author": an_agent_named("alice"),
        }),
    )
    .await;

    call("undo", undo_by("bob")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-renamed", "source"]
    );
    let alice_undo = call("undo", undo_by("alice")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-first", "source"]
    );
    assert_eq!(alice_undo.get("undo_state"), None, "{alice_undo}");
    let alice_redo = call("redo", undo_by("alice")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-renamed", "source"]
    );
    assert_eq!(alice_redo["undo_state"], "applied", "{alice_redo}");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["alice-renamed", "source"],
        "undo and redo apply to the running stream as well"
    );

    let nothing_left = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            undo_by("bob"),
        )
        .await,
    )
    .expect_err("bob's one edit is already undone");
    assert!(nothing_left.contains("bob"), "{nothing_left}");
}

/// A graph edit the runtime refuses, from any connection, is announced to a listen on the
/// project's change feed and read back from it tagged `refused`, newest first above the
/// transactions history records, with its author, the operations it asked for, its mode, its key
/// and the runtime's refusal; history records nothing for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_graph_edit_is_read_back_from_the_project_change_feed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let editing_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let listening_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let listed = tool_answer(
        &tool_call_over_the_connection(
            &editing_connection,
            "list_assets",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the project's assets list");
    let transactions_resource_uri = listed["transactions_resource_uri"]
        .as_str()
        .expect("the change feed's URI")
        .to_owned();
    let history_before = streamlib_project_repository::ProjectRepository::open(project.path())
        .expect("the project opens")
        .applied_transactions_newest_first(8)
        .expect("history reads");
    let mut subscription = listening_connection
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription(transactions_resource_uri.clone())
                .build(),
        )
        .await
        .expect("the runtime acknowledges a listen on the change feed");

    let absent_node = "33333333-3333-4333-8333-333333333333";
    let refused = tool_call_over_the_connection(
        &editing_connection,
        "remove_node",
        json!({
            "stream": stream_asset, "node": absent_node, "author": an_agent_named("alice"),
            "idempotency_key": "remove-gesture",
        }),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    let live_refused = tool_call_over_the_connection(
        &editing_connection,
        "rename_node",
        json!({
            "stream": stream_asset, "node": absent_node, "name": "glow", "mode": "live",
            "author": an_agent_named("bob"),
        }),
    )
    .await;
    assert_eq!(live_refused["isError"], true, "{live_refused}");

    let update = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the refusal is announced")
        .expect("the listen is still open")
        .expect("the update is well formed");
    let rmcp::model::ServerNotification::ResourceUpdatedNotification(update) = update else {
        panic!("expected notifications/resources/updated, got {update:?}");
    };
    assert_eq!(update.params.uri, transactions_resource_uri);

    let read = listening_connection
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            transactions_resource_uri.clone(),
        ))
        .await
        .expect("the change feed reads");
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    let entries = feed["transactions"].as_array().expect("the feed's entries");
    assert_eq!(entries.len(), history_before.len() + 2, "{feed}");
    let (newest, refused_before_it) = (&entries[0], &entries[1]);
    assert_eq!(newest["status"], "refused", "{feed}");
    assert_eq!(newest["mode"], "live");
    assert_eq!(newest["author_name"], "bob");
    assert_eq!(newest["refusal"]["action"], "rename_node");
    assert_eq!(refused_before_it["status"], "refused");
    assert_eq!(refused_before_it["author_kind"], "agent");
    assert_eq!(refused_before_it["author_name"], "alice");
    assert_eq!(refused_before_it["mode"], "commit");
    assert_eq!(refused_before_it["idempotency_key"], "remove-gesture");
    assert_eq!(
        refused_before_it["operations"],
        json!([{ "operation": "remove_node", "stream": stream_asset, "node": absent_node }])
    );
    assert_eq!(
        refused_before_it["refusal"], refused["structuredContent"]["refusal"],
        "the feed carries the refusal the caller was answered"
    );
    assert!(
        refused_before_it["refused_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    assert!(
        entries[2..]
            .iter()
            .all(|entry| entry["status"] == "applied"),
        "{feed}"
    );
    assert_eq!(
        streamlib_project_repository::ProjectRepository::open(project.path())
            .expect("the project opens")
            .applied_transactions_newest_first(8)
            .expect("history reads"),
        history_before,
        "history records no refusal"
    );
}

/// A committed edit sent twice under one idempotency key, as a client retrying after a timeout
/// would, applies once to the running stream and its graph document and answers the first
/// transaction; its commit records the format version and the key. An undo sent twice under
/// its own key undoes once.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_sent_again_under_its_idempotency_key_applies_once() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal}"))
        }
    };
    let add_the_sink = json!({
        "stream": stream_asset, "type": sink_type.as_str(), "name": "sink",
        "author": an_agent_named("alice"), "idempotency_key": "add-sink-gesture",
    });

    let first = call("add_node", add_the_sink.clone()).await;
    let retried = call("add_node", add_the_sink).await;
    assert_eq!(retried["transaction"], first["transaction"], "{retried}");
    assert_eq!(retried["commit"], first["commit"], "{retried}");
    assert_eq!(
        retried["added_nodes"][0]["node"],
        first["added_nodes"][0]["node"]
    );
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["sink", "source"]
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["sink", "source"],
        "the retry reached the running stream not at all"
    );

    let undo = json!({
        "project_directory": project.path(), "author": an_agent_named("alice"),
        "idempotency_key": "undo-gesture",
    });
    let undone = call("undo", undo.clone()).await;
    let undone_again = call("undo", undo).await;
    assert_eq!(undone_again["transaction"], undone["transaction"]);
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["source"]
    );

    let recorded = streamlib_project_repository::ProjectRepository::open(project.path())
        .expect("the project opens")
        .applied_transactions_newest_first(2)
        .expect("history reads");
    assert_eq!(recorded.len(), 2, "one add and one undo, each once");
    assert_eq!(
        recorded[1].transaction.format_version,
        streamlib_project_repository::PROJECT_TRANSACTION_FORMAT_VERSION
    );
    assert_eq!(
        recorded[1]
            .transaction
            .idempotency_key
            .as_ref()
            .map(|key| key.as_str()),
        Some("add-sink-gesture")
    );
    assert_eq!(undone["format_version"], json!(2), "{undone}");
    assert_eq!(undone["idempotency_key"], "undo-gesture", "{undone}");
    assert!(
        undone["applied_at"]
            .as_str()
            .is_some_and(|applied_at| applied_at.ends_with('Z')),
        "{undone}"
    );
}

/// A venv interpreter that answers the runtime's probe and, run as the graph generator run
/// entry, prints the project's `generator-run.json` as the run's document.
const STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN: &str = "#!/bin/sh\ncase \"$*\" in\n  *tatolab.stream._graph_generator_run_entry*) cat generator-run.json ;;\nesac\nexit 0\n";

/// The import path of the graph generator the stub interpreter's runs answer for.
const LOOK_GENERATOR: &str = "generators.look:look";

impl ProjectWithGraphDocuments {
    /// Give this project the interpreter of [`STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN`].
    fn with_a_generator_run_printed_from_generator_run_json(self) -> Self {
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_GENERATOR_RUN,
        );
        self
    }

    /// Make the next run of [`LOOK_GENERATOR`] place `fragment` as `look`.
    fn next_look_run_places(&self, fragment: Value) {
        std::fs::write(
            self.path().join("generator-run.json"),
            json!({
                "generator": LOOK_GENERATOR,
                "placed": { "name": "look", "type": LOOK_GENERATOR, "config": {}, "fragment": fragment },
            })
            .to_string(),
        )
        .expect("the generator run document");
    }

    /// Make the next run of [`LOOK_GENERATOR`] fail with `traceback`.
    fn next_look_run_fails_with(&self, traceback: &str) {
        std::fs::write(
            self.path().join("generator-run.json"),
            json!({ "generator": LOOK_GENERATOR, "failed_because": traceback }).to_string(),
        )
        .expect("the generator run document");
    }
}

/// A fragment of a source linked into a sink, and a spare sink when `with_a_spare` says so.
fn a_source_into_a_sink_fragment(with_a_spare: bool) -> Value {
    let source_type = StreamToolsTestSource::processor_class_import_path();
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let mut nodes = vec![
        json!({ "name": "pass-source", "type": source_type.as_str(), "config": {} }),
        json!({ "name": "pass-sink", "type": sink_type.as_str(), "config": {} }),
    ];
    if with_a_spare {
        nodes.push(json!({ "name": "pass-spare", "type": sink_type.as_str(), "config": {} }));
    }
    json!({
        "nodes": nodes,
        "links": [{
            "source": { "node": "pass-source", "port": "video" },
            "target": { "node": "pass-sink", "port": "video" },
        }],
    })
}

/// The UUID of each node of the fragment node `look` in `graph_document`, by name.
fn the_uuid_of_each_node_of_look_in(
    graph_document: &Value,
) -> std::collections::BTreeMap<String, String> {
    graph_document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["name"] == "look")
        .unwrap_or_else(|| panic!("the graph holds `look`: {graph_document}"))["fragment"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| {
            (
                node["name"].as_str().unwrap_or_default().to_owned(),
                node["id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// `import_graph_generator` places a generator's run as a fragment the running stream runs;
/// run again it refreshes that fragment, each node keeping its UUID by name, and one undo
/// brings the fragment back as it was.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_graph_generator_runs_as_a_fragment_and_a_rerun_keeps_each_nodes_uuid() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let import_the_look = || {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        let arguments = json!({
            "stream": stream_asset, "generator": LOOK_GENERATOR, "author": an_agent_named("alice"),
        });
        async move {
            tool_answer(
                &tool_call_result(
                    operations_on_the_loaded_streams,
                    "import_graph_generator",
                    arguments,
                )
                .await,
            )
        }
    };

    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    let placed = import_the_look().await.expect("the import applies");
    assert_eq!(placed["added_nodes"][0]["name"], "look", "{placed}");
    let first_run = project.the_graph_document_of("main");
    let first_uuids = the_uuid_of_each_node_of_look_in(&first_run);
    assert_eq!(
        first_uuids.keys().collect::<Vec<_>>(),
        ["pass-sink", "pass-source"],
        "{first_run}"
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["look", "source"],
        "the stream runs the fragment drawn as one node"
    );

    project.next_look_run_places(a_source_into_a_sink_fragment(true));
    let refreshed = import_the_look().await.expect("the rerun applies");
    assert_eq!(
        refreshed["operations"][0]["operation"], "refresh_fragment",
        "{refreshed}"
    );
    let second_run = project.the_graph_document_of("main");
    let second_uuids = the_uuid_of_each_node_of_look_in(&second_run);
    assert_eq!(second_uuids["pass-source"], first_uuids["pass-source"]);
    assert_eq!(second_uuids["pass-sink"], first_uuids["pass-sink"]);
    assert!(second_uuids.contains_key("pass-spare"), "{second_run}");
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&live),
        second_uuids,
        "the running stream runs the refreshed fragment: {live}"
    );

    project.next_look_run_fails_with("Traceback\nRuntimeError: the rig list is empty");
    let refusal = import_the_look()
        .await
        .expect_err("a failed run is refused");
    assert!(
        refusal.contains("RuntimeError: the rig list is empty"),
        "{refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), second_run);

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main")),
        first_uuids
    );
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&engine.the_live_graph_of(&stream_id)),
        first_uuids,
        "the undo brings the running fragment back as it was"
    );
}

/// A stream `create_stream` just made loads holding no node, and `import_graph_generator`
/// fills it as its first content.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_generator_seeds_a_stream_create_stream_just_made() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/looks.graph.json",
            "display_name": "Looks",
        }),
    )
    .await
    .expect("the stream is created");
    let load_of_the_created_stream = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        created["action"]
            .as_str()
            .expect("the creation queues a load"),
    )
    .await;
    assert_eq!(
        load_of_the_created_stream["state"], "done",
        "the empty stream loads: {load_of_the_created_stream}"
    );

    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    let placed = call(
        "import_graph_generator",
        json!({ "stream": created["asset"]["asset"], "generator": LOOK_GENERATOR }),
    )
    .await
    .expect("the generator seeds the new stream");
    assert_eq!(placed["added_nodes"][0]["name"], "look", "{placed}");
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("looks"))
            .keys()
            .collect::<Vec<_>>(),
        ["pass-sink", "pass-source"]
    );
}

/// An import under the name of a node that is no fragment is refused naming it, nothing changed.
#[tokio::test(flavor = "multi_thread")]
async fn an_import_under_the_name_of_a_node_that_is_no_fragment_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    std::fs::write(
        project.path().join("generator-run.json"),
        json!({
            "generator": LOOK_GENERATOR,
            "placed": { "name": "source", "type": LOOK_GENERATOR, "config": {},
                        "fragment": a_source_into_a_sink_fragment(false) },
        })
        .to_string(),
    )
    .expect("the generator run document");
    let before = project.the_graph_document_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR, "name": "source" }),
        )
        .await,
    )
    .expect_err("refused");

    assert!(
        refusal.contains(&format!("node {SOURCE_NODE_UUID} `source`"))
            && refusal.contains("no fragment"),
        "{refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), before);
}

/// `refresh_fragments` re-runs each fragment whose generator a changed module defines, keeping
/// each node's UUID by name, and answers no operation for a change no fragment's generator is in.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_fragments_reruns_the_fragments_a_changed_module_defines() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    project.next_look_run_places(a_source_into_a_sink_fragment(false));
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR }),
        )
        .await,
    )
    .expect("the import applies");
    let first_uuids = the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main"));
    let refresh_after_a_change_to = |changed_file: &'static str| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        let arguments = json!({
            "project_directory": project.path(), "changed_files": [changed_file],
        });
        async move {
            tool_answer(
                &tool_call_result(
                    operations_on_the_loaded_streams,
                    "refresh_fragments",
                    arguments,
                )
                .await,
            )
            .expect("the refresh applies")
        }
    };

    let untouched = refresh_after_a_change_to("nodes/blur.py").await;
    assert_eq!(untouched["operations"], json!([]), "{untouched}");
    assert_eq!(untouched["transaction"], json!(null), "{untouched}");

    project.next_look_run_places(a_source_into_a_sink_fragment(true));
    let refreshed = refresh_after_a_change_to("generators/look.py").await;

    assert_eq!(
        refreshed["operations"][0]["operation"], "refresh_fragment",
        "{refreshed}"
    );
    let refreshed_uuids = the_uuid_of_each_node_of_look_in(&project.the_graph_document_of("main"));
    assert_eq!(refreshed_uuids["pass-source"], first_uuids["pass-source"]);
    assert!(refreshed_uuids.contains_key("pass-spare"));
    assert_eq!(
        the_uuid_of_each_node_of_look_in(&engine.the_live_graph_of(&stream_id)),
        refreshed_uuids
    );
}

/// A `connect` to a port a fragment's `GraphInput` declares links the running stream to each
/// node that input feeds, and `graph` draws the link to the fragment.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_connected_to_a_fragments_port_feeds_the_nodes_its_input_feeds() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source()
        .with_a_generator_run_printed_from_generator_run_json();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    project.next_look_run_places(json!({
        "nodes": [
            { "name": "graphinput", "type": "tatolab.stream:GraphInput",
              "config": { "port": "video", "family": "image" } },
            { "name": "pass-sink", "type": sink_type.as_str(), "config": {} },
        ],
        "links": [{
            "source": { "node": "graphinput", "port": "value" },
            "target": { "node": "pass-sink", "port": "video" },
        }],
    }));
    let placed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_graph_generator",
            json!({ "stream": stream_asset, "generator": LOOK_GENERATOR }),
        )
        .await,
    )
    .expect("the import applies");
    let look = placed["added_nodes"][0]["node"].clone();

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "connect",
            json!({
                "stream": stream_asset,
                "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                "target": { "node": look, "port": "video" },
            }),
        )
        .await,
    )
    .expect("the connect applies");

    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live),
        ["source/video -> look/video"],
        "{live}"
    );
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        ["source/video -> look/video"]
    );
}

/// A labelled group is made, edited and taken out, each as one transaction kept in the graph
/// document and rendered by `graph`; a fresh runtime loading the document renders it too, and
/// undo of its removal brings it back with every node that belonged to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_labelled_group_survives_a_reload_and_undo_of_its_removal_restores_its_nodes() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_group",
        json!({ "stream": stream_asset, "title": "sources", "description": "every camera",
                "colour": "#3b82f6", "nodes": [SOURCE_NODE_UUID],
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group is made");
    let group = added["added_groups"][0]["group"]
        .as_str()
        .expect("add_group answers the group's UUID")
        .to_owned();
    assert_eq!(added["added_groups"][0]["title"], "sources", "{added}");
    call(
        "set_group_fields",
        json!({ "stream": stream_asset, "group": group, "colour": "#22c55e",
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group's fields are set");

    let the_group_as = json!([{ "id": group, "title": "sources",
                                "description": "every camera", "colour": "#22c55e" }]);
    let source_of = |graph: &Value| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == SOURCE_NODE_UUID)
            .cloned()
            .expect("the graph holds the source")
    };
    let kept = project.the_graph_document_of("main");
    assert_eq!(kept["groups"], the_group_as, "{kept}");
    assert_eq!(source_of(&kept)["studio"]["group"], json!(group), "{kept}");
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["groups"], the_group_as, "{live}");
    assert_eq!(source_of(&live)["studio"]["group"], json!(group), "{live}");
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(reloaded["groups"], the_group_as, "{reloaded}");

    let removed = call(
        "remove_group",
        json!({ "stream": stream_asset, "group": group, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the group is taken out");
    assert_eq!(
        removed["operations"][0]["nodes"],
        json!([SOURCE_NODE_UUID]),
        "{removed}"
    );
    let ungrouped = project.the_graph_document_of("main");
    assert!(ungrouped.get("groups").is_none(), "{ungrouped}");
    assert!(source_of(&ungrouped).get("studio").is_none(), "{ungrouped}");
    assert!(
        engine.the_live_graph_of(&stream_id).get("groups").is_none(),
        "the live graph still renders the group"
    );

    call(
        "undo",
        json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the undo applies");
    let restored = project.the_graph_document_of("main");
    assert_eq!(restored["groups"], the_group_as, "{restored}");
    assert_eq!(
        source_of(&restored)["studio"]["group"],
        json!(group),
        "{restored}"
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["groups"], the_group_as, "{live}");
    assert_eq!(source_of(&live)["studio"]["group"], json!(group), "{live}");
}

/// A canvas note is placed, edited and taken out, each as one transaction kept under the graph
/// document's `studio` and rendered by `graph`; a fresh runtime loading the document renders it
/// too, and undo of its removal brings it back. A node's studio fields are held to the studio
/// contract, a key outside it refused by name.
#[tokio::test(flavor = "multi_thread")]
async fn a_canvas_note_survives_a_reload_and_a_node_studio_key_outside_the_contract_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_note",
        json!({ "stream": stream_asset, "text": "check the exposure",
                "position": [40, 300], "size": [240, 120],
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note is placed");
    let note = added["added_notes"][0]["note"]
        .as_str()
        .expect("add_note answers the note's UUID")
        .to_owned();
    call(
        "set_note_fields",
        json!({ "stream": stream_asset, "note": note, "text": "check the white balance",
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note's fields are set");

    let the_studio_as = json!({ "contract": 1, "notes": [{
        "id": note, "text": "check the white balance",
        "position": [40, 300], "size": [240, 120] }] });
    assert_eq!(
        project.the_graph_document_of("main")["studio"],
        the_studio_as
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(live["studio"], the_studio_as, "{live}");
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(reloaded["studio"], the_studio_as, "{reloaded}");

    call(
        "remove_note",
        json!({ "stream": stream_asset, "note": note, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the note is taken out");
    assert!(
        project
            .the_graph_document_of("main")
            .get("studio")
            .is_none()
    );
    assert!(engine.the_live_graph_of(&stream_id).get("studio").is_none());
    call(
        "undo",
        json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the undo applies");
    assert_eq!(
        project.the_graph_document_of("main")["studio"],
        the_studio_as
    );
    assert_eq!(
        engine.the_live_graph_of(&stream_id)["studio"],
        the_studio_as
    );

    let refusal = call(
        "set_studio_fields",
        json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                "studio": { "position": [10, 20], "zoom": 2 } }),
    )
    .await
    .expect_err("a key outside the studio contract is refused");
    assert!(refusal.contains("`studio.zoom`"), "{refusal}");
    call(
        "set_studio_fields",
        json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID,
                "studio": { "position": [10, 20], "metadata": { "zoom": 2 } } }),
    )
    .await
    .expect("a client's own key under `metadata` is kept");
    let source = project.the_graph_document_of("main")["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == SOURCE_NODE_UUID)
        .cloned()
        .unwrap();
    assert_eq!(
        source["studio"],
        json!({ "contract": 1, "position": [10, 20], "metadata": { "zoom": 2 } })
    );
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// What the tool `tool_name` answers `arguments` with on this engine, or its refusal.
    async fn the_answer_of_the_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, String> {
        tool_answer(
            &tool_call_result(
                self.operations_on_the_loaded_streams(),
                tool_name,
                arguments,
            )
            .await,
        )
    }

    /// `author`'s undo of their latest transaction on `project`, or its refusal.
    async fn the_undo_by(
        &self,
        project: &ProjectWithGraphDocuments,
        author: &str,
    ) -> std::result::Result<Value, String> {
        self.the_answer_of_the_tool(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named(author) }),
        )
        .await
    }

    /// A [`StreamToolsTestSink`] `pass-sink` added to `project`'s `main` by `alice`, linked from
    /// its source: the sink's UUID.
    async fn a_sink_linked_from_the_source_of_main(
        &self,
        project: &ProjectWithGraphDocuments,
    ) -> String {
        let stream_asset = project.stream_asset_of("main");
        let added = self
            .the_answer_of_the_tool(
                "transact",
                json!({ "operations": [
                    { "operation": "add_node", "stream": stream_asset, "as": "sink",
                      "name": "pass-sink",
                      "type": StreamToolsTestSink::processor_class_import_path().as_str() },
                    { "operation": "connect", "stream": stream_asset,
                      "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                      "target": { "node": "sink", "port": "video" } },
                ], "author": an_agent_named("alice") }),
            )
            .await
            .expect("the sink is placed and linked");
        added["added_nodes"][0]["node"].as_str().unwrap().to_owned()
    }

    /// The node `node` of `project`'s `main` collapsed by `alice` into the subgraph `sub`: the
    /// subgraph node's UUID.
    async fn a_node_of_main_collapsed_into_sub(
        &self,
        project: &ProjectWithGraphDocuments,
        node: &str,
    ) -> String {
        let collapsed = self
            .the_answer_of_the_tool(
                "collapse_selection_into_subgraph",
                json!({ "stream": project.stream_asset_of("main"), "nodes": [node],
                        "name": "sub", "author": an_agent_named("alice") }),
            )
            .await
            .expect("the selection collapses");
        assert_eq!(collapsed["added_nodes"][0]["name"], "sub", "{collapsed}");
        collapsed["added_nodes"][0]["node"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// The stream asset the stream fragment node `placing_node` of `document` places.
fn the_stream_asset_placed_by(document: &Value, placing_node: &str) -> String {
    document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["id"] == placing_node)
        .and_then(|node| node["config"]["stream"]["asset"].as_str())
        .unwrap_or_else(|| panic!("node {placing_node} places no stream: {document}"))
        .to_owned()
}

/// A sink collapsed into a subgraph is saved as the project's stream `sub` and runs where `sub`
/// is placed, fed through its port from the same source; a name a stream already holds is
/// refused; unlocking gives back a copy of the sink linked straight to the source; undoing the
/// unlock and the collapse gives back the exact earlier document and takes `sub` out, and a redo
/// saves it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_collapsed_selection_is_saved_as_a_stream_running_where_it_is_placed_and_undoes_exactly()
{
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let before_the_collapse = project.the_graph_document_of("main");

    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;

    let collapsed = project.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&collapsed), ["source", "sub"]);
    let placing_node = the_node_named(&collapsed, "sub");
    assert_eq!(placing_node["type"], STREAM_FRAGMENT_NODE_TYPE);
    assert!(
        placing_node.get("fragment").is_none(),
        "main stores the reference alone: {placing_node}"
    );
    assert_eq!(
        links_by_node_name_in(&collapsed),
        ["source/video -> sub/pass-sink-video"]
    );
    let saved = project.the_graph_document_of("sub");
    assert_eq!(
        saved["asset"].as_str().unwrap(),
        the_stream_asset_placed_by(&collapsed, &subgraph)
    );
    assert_eq!(
        sorted_node_names_in(&saved),
        ["pass-sink", "pass-sink-video"]
    );
    assert_eq!(the_node_named(&saved, "pass-sink")["id"], json!(sink));
    assert_eq!(
        links_by_node_name_in(&saved),
        ["pass-sink-video/value -> pass-sink/video"]
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live),
        ["source/video -> sub/pass-sink-video"],
        "{live}"
    );
    let live_subgraph = the_node_named(&live, "sub");
    assert_eq!(
        the_node_names_a_fragment_places(&live_subgraph),
        ["pass-sink", "pass-sink-video"],
        "{live_subgraph}"
    );
    assert_eq!(
        the_node_named(&live_subgraph["fragment"], "pass-sink")["id"],
        json!(the_uuid_a_held_node_takes_under(&subgraph, &sink)),
        "{live_subgraph}"
    );
    assert!(
        live["links"][0]["id"]
            .as_str()
            .is_some_and(|link_id| !link_id.is_empty()),
        "the drawn link into the subgraph renders the running link it stands for: {live}"
    );

    let refused = engine
        .the_answer_of_the_tool(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [SOURCE_NODE_UUID], "name": "main",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a stream's name is not taken twice");
    assert!(refused.contains("streams/main.graph.json"), "{refused}");

    engine
        .the_answer_of_the_tool(
            "unlock_subgraph",
            json!({ "stream": stream_asset, "fragment": subgraph,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the subgraph unlocks");
    let unlocked = project.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&unlocked), ["pass-sink", "source"]);
    assert_ne!(
        the_node_named(&unlocked, "pass-sink")["id"],
        json!(sink),
        "the unlocked sink is a copy under a fresh UUID"
    );
    assert_eq!(
        links_by_node_name_in(&unlocked),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        project.the_graph_document_of("sub"),
        saved,
        "the unlocked stream stays a stream of the project"
    );

    for _ in 0..2 {
        engine
            .the_undo_by(&project, "alice")
            .await
            .expect("the undo applies");
    }
    assert_eq!(project.the_graph_document_of("main"), before_the_collapse);
    assert!(
        !project.path().join("streams/sub.graph.json").exists(),
        "the undo of the collapse takes the saved stream out"
    );
    let live_once_undone = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live_once_undone),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        the_node_named(&live_once_undone, "pass-sink")["id"],
        json!(sink),
        "{live_once_undone}"
    );

    engine
        .the_answer_of_the_tool(
            "redo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the redo applies");
    assert_eq!(project.the_graph_document_of("sub"), saved);
    assert_eq!(project.the_graph_document_of("main"), collapsed);
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "sub"
        )),
        ["pass-sink", "pass-sink-video"]
    );
}

/// An undo of a collapse is refused while another stream of the project places the subgraph it
/// saved, the refusal naming that stream, and goes through once that placement is undone.
#[tokio::test(flavor = "multi_thread")]
async fn the_undo_of_a_collapse_is_refused_while_another_stream_places_its_subgraph() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let mut other = the_graph_of_a_stream_named("other");
    other["nodes"][0]["id"] = json!(Uuid::new_v4());
    project.write_the_graph_document(other);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    engine.a_stream_loaded_from_its_graph_document(&project, "other");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let before_the_collapse = project.the_graph_document_of("main");
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("other"),
                    "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub",
                    "config": { "stream": { "asset": sub_asset } },
                    "author": an_agent_named("bob") }),
        )
        .await
        .expect("bob places sub in other");

    let refused = engine
        .the_undo_by(&project, "alice")
        .await
        .expect_err("alice's collapse is not undone while other places sub");

    assert!(refused.contains("streams/other.graph.json"), "{refused}");
    assert!(project.path().join("streams/sub.graph.json").exists());
    engine
        .the_undo_by(&project, "bob")
        .await
        .expect("bob's placement undoes");
    engine
        .the_undo_by(&project, "alice")
        .await
        .expect("alice's collapse undoes once nothing else places sub");
    assert_eq!(project.the_graph_document_of("main"), before_the_collapse);
    assert!(!project.path().join("streams/sub.graph.json").exists());
}

/// A node added, wired and renamed at the level of a stream `main` places twice is made on that
/// stream's own document and runs in both placements; renaming one of its ports from inside
/// carries the links onto it in both; each edit undoes to the exact earlier documents.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_inside_a_stream_placed_twice_reaches_both_placements_and_undoes_exactly() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path()
        .as_str()
        .to_owned();
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    let graph_input = the_node_named(&project.the_graph_document_of("sub"), "pass-sink-video");
    let graph_input_drawn =
        the_uuid_a_held_node_takes_under(&subgraph, graph_input["id"].as_str().unwrap());
    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "as": "again",
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub-again",
                  "config": { "stream": { "asset": sub_asset } } },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                  "target": { "node": "again", "port": "pass-sink-video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("main places sub a second time");
    let main_before_the_edits = project.the_graph_document_of("main");
    let sub_before_the_edits = project.the_graph_document_of("sub");

    let added_inside = engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "level": [subgraph],
                  "as": "second", "type": sink_type },
                { "operation": "connect", "stream": stream_asset, "level": [subgraph],
                  "source": { "node": graph_input_drawn, "port": "value" },
                  "target": { "node": "second", "port": "video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("a node is added and wired inside sub");
    let second_drawn = added_inside["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        added_inside["added_nodes"][0]["name"], "streamtoolstestsink",
        "{added_inside}"
    );
    engine
        .the_answer_of_the_tool(
            "rename_node",
            json!({ "stream": stream_asset, "level": [subgraph], "node": second_drawn,
                    "name": "second-sink", "author": an_agent_named("alice") }),
        )
        .await
        .expect("the node inside sub is renamed");

    assert_eq!(project.the_graph_document_of("main"), main_before_the_edits);
    let sub = project.the_graph_document_of("sub");
    assert_eq!(
        sorted_node_names_in(&sub),
        ["pass-sink", "pass-sink-video", "second-sink"]
    );
    let second_own = the_node_named(&sub, "second-sink")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        the_uuid_a_held_node_takes_under(&subgraph, &second_own),
        second_drawn,
        "the answer names the node as main draws it"
    );
    let live = engine.the_live_graph_of(&stream_id);
    for placement in ["sub", "sub-again"] {
        let live_placement = the_node_named(&live, placement);
        assert_eq!(
            the_node_names_a_fragment_places(&live_placement),
            ["pass-sink", "pass-sink-video", "second-sink"],
            "{live_placement}"
        );
        assert_eq!(
            links_by_node_name_in(&live_placement["fragment"]),
            [
                "pass-sink-video/value -> pass-sink/video",
                "pass-sink-video/value -> second-sink/video"
            ],
            "{live_placement}"
        );
    }

    let mut renamed_port_config = graph_input["config"].clone();
    renamed_port_config["port"] = json!("video_in");
    engine
        .the_answer_of_the_tool(
            "set_node_config",
            json!({ "stream": stream_asset, "level": [subgraph], "node": graph_input_drawn,
                    "config": renamed_port_config, "author": an_agent_named("alice") }),
        )
        .await
        .expect("sub's port is renamed from inside it");
    let links_onto_the_renamed_port = [
        "source/video -> sub/video_in",
        "source/video -> sub-again/video_in",
    ];
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        links_onto_the_renamed_port,
        "the links onto the renamed port follow it in both placements"
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        links_onto_the_renamed_port,
        "the running links onto the renamed port follow it in both placements"
    );

    for _ in 0..3 {
        engine
            .the_undo_by(&project, "alice")
            .await
            .expect("the undo applies");
    }
    assert_eq!(project.the_graph_document_of("main"), main_before_the_edits);
    assert_eq!(project.the_graph_document_of("sub"), sub_before_the_edits);
    let live_once_undone = engine.the_live_graph_of(&stream_id);
    for placement in ["sub", "sub-again"] {
        assert_eq!(
            the_node_names_a_fragment_places(&the_node_named(&live_once_undone, placement)),
            ["pass-sink", "pass-sink-video"]
        );
    }
}

/// A transaction that collapses a selection and then renames the subgraph by its label reaches
/// the running stream in that order, and so does its redo.
#[tokio::test(flavor = "multi_thread")]
async fn a_collapse_and_a_rename_of_its_subgraph_in_one_transaction_reach_the_running_stream_in_order()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let running_look_places_the_sink = |live: &Value| {
        assert_eq!(sorted_node_names_in(live), ["look", "source"], "{live}");
        assert_eq!(
            the_node_names_a_fragment_places(&the_node_named(live, "look")),
            ["pass-sink", "pass-sink-video"]
        );
    };

    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [sink], "name": "sub", "as": "subgraph" },
                { "operation": "rename_node", "stream": stream_asset, "node": "subgraph",
                  "name": "look" },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("the collapse and the rename apply in order");
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["look", "source"]
    );
    running_look_places_the_sink(&engine.the_live_graph_of(&stream_id));

    engine
        .the_undo_by(&project, "alice")
        .await
        .expect("the transaction undoes");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["pass-sink", "source"]
    );
    engine
        .the_answer_of_the_tool(
            "redo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the redo applies in order");
    running_look_places_the_sink(&engine.the_live_graph_of(&stream_id));
}

/// An add inside a placed stream of a type the stream does not resolve is refused, as the same
/// add at the stream's own level is, and leaves the placed stream as it was.
#[tokio::test(flavor = "multi_thread")]
async fn an_add_inside_a_placed_stream_of_a_type_nothing_resolves_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_before_the_add = project.the_graph_document_of("sub");
    let unresolved_type = StreamToolsTestSink::processor_class_import_path()
        .as_str()
        .replace("StreamToolsTestSink", "StreamToolsTestSinkNothingRegisters");

    let refused = engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("main"), "level": [subgraph],
                    "type": unresolved_type, "name": "missing",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a type nothing resolves is not added inside sub");

    assert!(
        refused.contains("StreamToolsTestSinkNothingRegisters"),
        "{refused}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before_the_add);
    engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("main"), "level": [subgraph],
                    "type": GRAPH_OUTPUT_NODE_TYPE, "name": "out",
                    "config": { "port": "out", "family": "image" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("a Graph Output is added inside sub");
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("sub")),
        ["out", "pass-sink", "pass-sink-video"]
    );
}

/// A transaction that collapses one placement of `sub` into a new stream and then renames a
/// port of `sub` from inside its other placement carries the link onto that port in the new
/// stream too.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_renamed_inside_a_placed_stream_is_carried_in_a_stream_the_same_transaction_saves() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    let placed_again = engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "as": "again",
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub-again",
                  "config": { "stream": { "asset": sub_asset } } },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                  "target": { "node": "again", "port": "pass-sink-video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("main places sub a second time");
    let sub_again = placed_again["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    let graph_input = the_node_named(&project.the_graph_document_of("sub"), "pass-sink-video");
    let mut renamed_port_config = graph_input["config"].clone();
    renamed_port_config["port"] = json!("video_in");

    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [subgraph], "name": "wrapper" },
                { "operation": "set_node_config", "stream": stream_asset, "level": [sub_again],
                  "node": the_uuid_a_held_node_takes_under(
                      &sub_again, graph_input["id"].as_str().unwrap()),
                  "config": renamed_port_config },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("the collapse and the rename apply");

    let links_into_sub_in_wrapper: Vec<String> =
        links_by_node_name_in(&project.the_graph_document_of("wrapper"))
            .into_iter()
            .filter(|link| link.contains("-> sub/"))
            .collect();
    assert_eq!(
        links_into_sub_in_wrapper.len(),
        1,
        "{links_into_sub_in_wrapper:?}"
    );
    assert!(
        links_into_sub_in_wrapper[0].ends_with("-> sub/video_in"),
        "{links_into_sub_in_wrapper:?}"
    );
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main"))
            .into_iter()
            .filter(|link| link.contains("sub-again"))
            .collect::<Vec<_>>(),
        ["source/video -> sub-again/video_in"]
    );
}

/// Unlocking the placement of an import's stream copies its nodes into the project with each
/// stream they place still read from the import.
#[tokio::test(flavor = "multi_thread")]
async fn an_unlocked_placement_of_an_imports_stream_keeps_each_stream_it_places_in_the_import() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{ "id": Uuid::new_v4(), "name": "grain", "type": STREAM_FRAGMENT_NODE_TYPE,
                    "config": {} }],
        "links": [],
    }));
    let grain_asset = arm.stream_asset_of("grain");
    let mut arm_main = arm.the_graph_document_of("main");
    arm_main["nodes"][0]["config"] = json!({ "stream": { "asset": grain_asset } });
    arm.write_the_graph_document_text("main", &arm_main.to_string());
    let mut grain = the_graph_of_a_stream_named("grain");
    grain["nodes"][0]["id"] = json!(Uuid::new_v4());
    arm.write_the_graph_document(grain);
    let cell = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let stream_asset = cell.stream_asset_of("main");
    engine
        .the_answer_of_the_tool(
            "add_dependency",
            json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm is imported");
    let used = engine
        .the_answer_of_the_tool(
            "use_dependency_stream",
            json!({ "stream": stream_asset, "project": arm.project,
                    "dependency_stream": arm.stream_asset_of("main").to_string(), "name": "arm",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm's stream is placed");
    let fragment = used["added_nodes"][0]["node"].as_str().unwrap().to_owned();

    engine
        .the_answer_of_the_tool(
            "unlock_subgraph",
            json!({ "stream": stream_asset, "fragment": fragment,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm's placement unlocks");

    let unlocked = cell.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&unlocked), ["grain", "source"]);
    assert_eq!(
        the_node_named(&unlocked, "grain")["config"],
        json!({ "stream": { "asset": grain_asset, "from": arm.project } })
    );
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "grain"
        )),
        ["source"]
    );
}

impl ProjectWithGraphDocuments {
    /// Add a [`StreamToolsTestSink`] named `name` to this project's stream `main` as one
    /// committed transaction, answering the commit that records it.
    fn commit_a_sink_named(&self, name: &str) -> String {
        register_the_stream_tools_test_sink_once();
        let mut project_repository =
            streamlib_project_repository::ProjectRepository::open(self.path())
                .expect("the project opens");
        project_repository
            .apply_project_operations(
                &streamlib_project_repository::ProjectTransactionAuthor {
                    kind: streamlib_project_repository::ProjectTransactionAuthorKind::Person,
                    name: "arm maintainer".to_owned(),
                },
                None,
                Vec::new(),
                vec![streamlib_project_repository::ProjectOperation::StreamGraph(
                    streamlib_project_repository::StreamGraphOperationOnAStream {
                        stream: self.stream_asset_of("main"),
                        graph_operation:
                            streamlib_project_repository::StreamGraphOperation::AddNode {
                                node: streamlib_project_repository::StreamGraphDocumentNode {
                                    id: Uuid::new_v4(),
                                    name: name.to_owned(),
                                    node_type: StreamToolsTestSink::processor_class_import_path()
                                        .as_str()
                                        .to_owned(),
                                    config: json!({}),
                                    pins: serde_json::Map::new(),
                                    exposed_config_fields: Vec::new(),
                                    studio: Default::default(),
                                    fragment: None,
                                },
                                links: Vec::new(),
                            },
                    },
                )],
                streamlib_project_repository::ProjectTransactionLineage::NewEdit,
            )
            .expect("the sink is committed")
            .into_applied_transaction()
            .commit
    }
}

/// The node named `name` of the graph document or render `graph`.
fn the_node_named(graph: &Value, name: &str) -> Value {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("the graph holds no node `{name}`: {graph}"))
}

/// The names of the nodes the fragment of `fragment_node` places, in order.
fn the_node_names_a_fragment_places(fragment_node: &Value) -> Vec<String> {
    fragment_node["fragment"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| node["name"].as_str().map(str::to_owned))
        .collect()
}

/// The UUID a node of a held stream takes in the fragment `fragment_node` places it under.
fn the_uuid_a_held_node_takes_under(fragment_node: &str, held_node: &str) -> String {
    Uuid::new_v5(
        &Uuid::parse_str(fragment_node).expect("the fragment node's UUID"),
        Uuid::parse_str(held_node)
            .expect("the held node's UUID")
            .as_bytes(),
    )
    .to_string()
}

/// Project `cell` imports project `arm` and places `arm`'s stream as a fragment: the graph
/// document stores the reference alone, the live graph places the stream's nodes under UUIDv5
/// ids, an update over an edited folder waits for its confirmation, the confirmed update and
/// its undo reach the running stream, a fresh load places the import's stream from disk, and
/// the import is removed once no node references it.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_stream_placed_as_a_fragment_follows_its_folder_through_an_update_and_an_undo()
{
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = a_project_whose_main_stream_holds_one_source();
    arm.commit_a_sink_named("shoulder");
    let shoulder = the_node_named(&arm.the_graph_document_of("main"), "shoulder")["id"]
        .as_str()
        .expect("the arm's shoulder holds a UUID")
        .to_owned();
    let cell = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let stream_asset = cell.stream_asset_of("main");
    let arm_stream = arm.stream_asset_of("main").to_string();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm is imported");
    let folder = added["dependency"]["folder"]
        .as_str()
        .expect("add_dependency answers the import's folder")
        .to_owned();
    let import_folder = cell.path().join("imports").join(&folder);
    assert!(
        import_folder.join("streams/main.graph.json").is_file(),
        "{added}"
    );
    let first_version = added["dependency"]["version"].clone();

    let used = call(
        "use_dependency_stream",
        json!({ "stream": stream_asset, "project": arm.project, "dependency_stream": arm_stream,
                "name": "arm", "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm's stream is placed");
    let fragment = used["added_nodes"][0]["node"]
        .as_str()
        .expect("use_dependency_stream answers the fragment node's UUID")
        .to_owned();
    let stored = the_node_named(&cell.the_graph_document_of("main"), "arm");
    assert_eq!(
        stored["config"],
        json!({ "stream": { "asset": arm_stream, "from": arm.project } }),
        "{stored}"
    );
    assert!(stored.get("fragment").is_none(), "{stored}");
    let live = the_node_named(&engine.the_live_graph_of(&stream_id), "arm");
    assert_eq!(
        the_node_names_a_fragment_places(&live),
        ["source", "shoulder"]
    );
    assert_eq!(
        the_node_named(&live["fragment"], "shoulder")["id"],
        json!(the_uuid_a_held_node_takes_under(&fragment, &shoulder)),
        "{live}"
    );
    let placed_again = call(
        "use_dependency_stream",
        json!({ "stream": stream_asset, "project": arm.project, "dependency_stream": arm_stream,
                "name": "arm", "author": an_agent_named("alice") }),
    )
    .await
    .expect_err("a name the graph already holds is not placed again");
    assert!(placed_again.contains("already places"), "{placed_again}");

    arm.commit_a_sink_named("elbow");
    let listed = call(
        "list_dependencies",
        json!({ "project_directory": cell.path(), "check_for_newer": true }),
    )
    .await
    .expect("the imports are listed");
    assert_eq!(
        listed["dependencies"][0]["dependency"]["version"], first_version,
        "{listed}"
    );
    assert_eq!(
        listed["dependencies"][0]["newer_version_exists"],
        json!(true),
        "{listed}"
    );
    let refused_while_used = call(
        "remove_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect_err("an import a fragment references is not removed");
    assert!(
        refused_while_used.contains(&fragment),
        "{refused_while_used}"
    );

    std::fs::write(import_folder.join("notes.txt"), "edited in place\n")
        .expect("a file written into the import's folder");
    let waiting = call(
        "update_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("an update over edited files answers them");
    assert_eq!(
        waiting["overwrite_confirmation_needed"],
        json!(true),
        "{waiting}"
    );
    assert_eq!(waiting["edited_files"], json!(["notes.txt"]), "{waiting}");
    assert!(waiting.get("commit").is_none(), "{waiting}");
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder"]
    );

    let updated = call(
        "update_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "overwrite_edited_files": true, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the import is copied again");
    assert_ne!(updated["dependency"]["version"], first_version, "{updated}");
    assert!(!import_folder.join("notes.txt").exists());
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder", "elbow"]
    );
    assert!(
        the_node_named(&cell.the_graph_document_of("main"), "arm")
            .get("fragment")
            .is_none()
    );

    call(
        "undo",
        json!({ "project_directory": cell.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the update is undone");
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder"]
    );
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&cell, "main");
    let reloaded = the_node_named(&restarted.the_live_graph_of(&reloaded_stream_id), "arm");
    assert_eq!(
        the_node_names_a_fragment_places(&reloaded),
        ["source", "shoulder"]
    );

    call(
        "remove_node",
        json!({ "stream": stream_asset, "node": fragment, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the fragment is taken out");
    let removed = call(
        "remove_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("an import no node references any longer is removed");
    assert_eq!(removed["dependency"]["folder"], json!(folder), "{removed}");
    assert!(!import_folder.exists());
    let listed_after = call(
        "list_dependencies",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the imports are listed");
    assert_eq!(listed_after["dependencies"], json!([]), "{listed_after}");
}

/// A venv interpreter whose node type listing keeps the request it was sent in
/// `listing-request.json` and prints `listing.json`, which a test writes first.
const STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING: &str = "#!/bin/sh\ncase \"$*\" in\n  *tatolab.stream._project_node_type_listing_entry*) cat > listing-request.json; cat listing.json ;;\nesac\nexit 0\n";

/// Project `cell` imports project `arm`: the listing is sent the Python files of `arm`'s folder
/// under `imports/` as they are on disk and nothing else of it, and answers each source as the
/// project's interpreter read it, under the project's UUID and name.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_node_types_are_listed_with_each_imports_python_sources_in_its_folder() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::create_dir_all(arm.path().join("nodes")).expect("the arm's nodes directory");
    std::fs::write(
        arm.path().join("nodes/gripper.py"),
        "from tatolab.stream import node\n",
    )
    .expect("the arm's node code");
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let project_source = json!({
        "source": "project",
        "node_types": [{
            "type": "nodes.ik:InverseKinematics", "kind": "process", "description": "",
            "category": "nodes.ik", "inputs": [], "outputs": [], "path": "nodes/ik.py", "line": 4,
        }],
        "graph_generators": [],
        "unreadable_modules": [{
            "module": "nodes.broken", "path": "nodes/broken.py", "line": 2, "column": 7,
            "why": "SyntaxError: invalid syntax",
        }],
    });
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };
    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm is imported");
    let folder = added["dependency"]["folder"].clone();
    let version = added["dependency"]["version"].clone();
    let dependency_source = json!({
        "source": "dependency", "project": arm.project, "folder": folder, "version": version,
        "node_types": [], "graph_generators": [], "unreadable_modules": [],
    });
    std::fs::write(
        cell.path().join("listing.json"),
        json!({ "sources": [project_source, dependency_source] }).to_string(),
    )
    .expect("the listing document");

    let listed = call(
        "list_project_node_types",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the cell's node types are listed");

    assert_eq!(listed["project"], json!(cell.project), "{listed}");
    assert_eq!(listed["project_name"], TEST_PROJECT_NAME, "{listed}");
    assert_eq!(
        listed["sources"],
        json!([project_source, dependency_source]),
        "{listed}"
    );
    let request: Value = serde_json::from_slice(
        &std::fs::read(cell.path().join("listing-request.json")).expect("the listing's request"),
    )
    .expect("the request is JSON");
    assert_eq!(
        request,
        json!({ "dependencies": [{
            "project": arm.project, "folder": folder, "version": version,
            "files": { "nodes/gripper.py": "from tatolab.stream import node\n" },
        }] })
    );
}

/// Write `manifest` as the `tatolab_view.json` of `views/<view_folder_name>/` in `project_directory`.
fn a_view_folder_in(project_directory: &Path, view_folder_name: &str, manifest: &Value) {
    let folder = project_directory.join("views").join(view_folder_name);
    std::fs::create_dir_all(&folder).expect("the view's folder");
    std::fs::write(folder.join("tatolab_view.json"), manifest.to_string()).expect("its manifest");
}

/// Project `cell` brings the view `deck` and imports project `levels`, which brings `meter` and a
/// folder whose manifest is not JSON: each is listed under the project bringing it, and the one
/// that does not open says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_views_and_its_imports_views_are_listed_by_view_type() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let levels = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::write(
        levels.path().join("tatolab-project.json"),
        json!({ "project": levels.project, "name": "levels", "dependencies": [] }).to_string(),
    )
    .expect("the import's project file names it");
    a_view_folder_in(
        levels.path(),
        "meter",
        &json!({ "contract": "1.0", "title": "Meter", "entry": "index.html", "placement": "area" }),
    );
    let broken_folder = levels.path().join("views/broken");
    std::fs::create_dir_all(&broken_folder).expect("the broken view's folder");
    std::fs::write(broken_folder.join("tatolab_view.json"), "{").expect("a manifest cut short");
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let deck_manifest = json!({
        "contract": "1.0", "title": "DJ Deck", "description": "Two decks and a crossfader.",
        "icon": "icon.svg", "preview": "preview.png", "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "waveform", "family": "channels" }],
        "outputs": [{ "name": "crossfader", "family": "array", "range": [-1, 1] }],
    });
    a_view_folder_in(cell.path(), "deck", &deck_manifest);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };
    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": levels.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("levels is imported");
    let folder = added["dependency"]["folder"].clone();

    let listed = call(
        "list_project_views",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the cell's views are listed");

    assert_eq!(listed["project"], json!(cell.project), "{listed}");
    assert_eq!(listed["project_name"], TEST_PROJECT_NAME, "{listed}");
    let views = listed["views"].as_array().expect("a list of views");
    assert_eq!(views.len(), 3, "{listed}");
    assert_eq!(
        views[0],
        json!({
            "view_type": format!("{TEST_PROJECT_NAME}:deck"),
            "project": cell.project, "project_name": TEST_PROJECT_NAME, "import_folder": null,
            "view_folder_in_the_project": "views/deck",
            "opens": true, "not_opened_because": null,
            "title": "DJ Deck", "description": "Two decks and a crossfader.",
            "icon": "icon.svg", "preview": "preview.png", "placement": "node",
            "inputs": [{ "name": "waveform", "family": "channels", "description": null, "range": null }],
            "outputs": [{ "name": "crossfader", "family": "array", "description": null, "range": [-1.0, 1.0] }],
            "manifest": deck_manifest,
        })
    );
    assert_eq!(views[1]["view_type"], "levels:broken", "{listed}");
    assert_eq!(views[1]["opens"], false, "{listed}");
    assert_eq!(views[1]["import_folder"], folder, "{listed}");
    assert!(
        views[1]["not_opened_because"]
            .as_str()
            .is_some_and(|why| why.starts_with("`tatolab_view.json` is not JSON: ")),
        "{listed}"
    );
    assert_eq!(views[2]["view_type"], "levels:meter", "{listed}");
    assert_eq!(views[2]["project"], json!(levels.project), "{listed}");
    assert_eq!(views[2]["project_name"], "levels", "{listed}");
    assert_eq!(views[2]["placement"], "area", "{listed}");
    assert_eq!(
        views[2]["view_folder_in_the_project"],
        json!(format!(
            "imports/{}/views/meter",
            folder.as_str().expect("a folder name")
        )),
        "{listed}"
    );
}

/// A listing whose interpreter prints no listing is refused naming the project, in the
/// runtime's own words.
/// The UUID the graph document of the view-node test gives its `deck`.
const DECK_NODE_UUID: &str = "decdecde-cdec-4dec-8dec-decdecdecdec";

/// A graph document holding a node of the project's view `deck` loads it with the view's ports,
/// and a panel's write onto it is live data alone: refused while the node does not run, and the
/// graph document on disk keeps nothing of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_document_holding_a_view_node_loads_it_with_the_views_ports_and_a_write_records_nothing()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    register_the_stream_tools_test_source_once();
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"].as_array_mut().unwrap().push(json!({
        "id": DECK_NODE_UUID, "name": "deck", "type": format!("{TEST_PROJECT_NAME}:deck"),
        "config": { "bpm": 128 },
    }));
    let project = ProjectWithGraphDocuments::holding(graph);
    a_view_folder_in(
        project.path(),
        "deck",
        &json!({
            "contract": "1.0", "title": "Deck", "entry": "index.html", "placement": "node",
            "inputs": [{ "name": "waveform", "family": "channels" }],
            "outputs": [{ "name": "crossfader", "family": "array", "range": [-1, 1] }],
        }),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let document_before = project.the_graph_document_of("main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let deck = the_node_named(&live_graph, "deck");
    assert_eq!(deck["id"], DECK_NODE_UUID, "{deck}");
    assert_eq!(deck["type"], format!("{TEST_PROJECT_NAME}:deck"), "{deck}");
    assert!(
        deck.to_string().contains("waveform") && deck.to_string().contains("crossfader"),
        "the view node takes the view's ports: {deck}"
    );

    let refused_result = tool_call_result(
        engine.operations_on_the_loaded_streams(),
        "write_studio_view_output",
        json!({ "stream": stream_id, "node": DECK_NODE_UUID, "port": "crossfader",
                "value": 0.5 }),
    )
    .await;
    assert_eq!(
        refused_result["structuredContent"]["refusal"]["not_running_because"], "preparing",
        "{refused_result}"
    );
    let refusal = tool_answer(&refused_result).expect_err("a node not running publishes nothing");
    assert!(
        refusal.starts_with("write_studio_view_output failed:") && refusal.contains("not running"),
        "{refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// A native reader of Records on its input `cue`, which does nothing with them.
#[streamlib::sdk::processor(
    execution = manual,
    input("cue", delivery_profile = "newest", family = Records)
)]
pub struct StreamToolsTestRecordsReader;

impl ManualProcessor for StreamToolsTestRecordsReader::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// The UUID the graph document of the Graph Input test gives its `cue` Input.
const CUE_GRAPH_INPUT_NODE_UUID: &str = "c0ec0ec0-ec0e-4c0e-8c0e-c0ec0ec0ec0e";

/// The graph of `main` with a top-level Records `GraphInput` declaring `cue`, holding `default`
/// as its config's, read by two [`StreamToolsTestRecordsReader`]s.
fn the_graph_of_a_stream_whose_cue_input_two_nodes_read(default: Value) -> Value {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED
        .call_once(|| PROCESSOR_REGISTRY.register::<StreamToolsTestRecordsReader::Processor>());
    let reader_type = StreamToolsTestRecordsReader::processor_class_import_path();
    json!({
        "stream": "main",
        "nodes": [
            { "id": CUE_GRAPH_INPUT_NODE_UUID, "name": "cue", "type": "tatolab.stream:GraphInput",
              "config": { "port": "cue", "family": "records", "default": default } },
            { "name": "left", "type": reader_type.as_str(), "config": {} },
            { "name": "right", "type": reader_type.as_str(), "config": {} },
        ],
        "links": [
            { "source": { "node": "cue", "port": "value" }, "target": { "node": "left", "port": "cue" } },
            { "source": { "node": "cue", "port": "value" }, "target": { "node": "right", "port": "cue" } },
        ],
    })
}

/// A stream's top-level Records `GraphInput` loads as a running node linked to both its readers,
/// rendered as an interface node carrying its default; `write_graph_input` names it by its
/// Input, and a refused write records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_top_level_graph_input_loads_linked_to_its_readers_and_a_write_records_nothing() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_whose_cue_input_two_nodes_read(json!({ "track": "intro" })),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let document_before = project.the_graph_document_of("main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let cue = the_node_named(&live_graph, "cue");
    assert_eq!(cue["id"], CUE_GRAPH_INPUT_NODE_UUID, "{cue}");
    assert_eq!(
        cue["config"]["default"],
        json!({ "track": "intro" }),
        "{cue}"
    );
    assert_eq!(
        cue["components"]["graph_interface"],
        json!({
            "direction": "input",
            "port": "cue",
            "family": "records",
            "current_value": { "track": "intro" }
        }),
        "a node not running shows the value it carries at its next Play, its default: {cue}"
    );
    assert_eq!(cue["ports"]["outputs"][0]["name"], "value", "{cue}");
    let links_out_of_the_input: Vec<&Value> = live_graph["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|link| link["source"]["node"] == "cue")
        .collect();
    assert_eq!(links_out_of_the_input.len(), 2, "{live_graph}");
    for link in links_out_of_the_input {
        assert!(
            !link["id"].as_str().unwrap_or_default().is_empty(),
            "a running Input's links are links the runtime runs: {link}"
        );
        assert_eq!(
            link["components"]["graph_interface"]["port"], "cue",
            "{link}"
        );
    }

    let refused_result = tool_call_result(
        engine.operations_on_the_loaded_streams(),
        "write_graph_input",
        json!({ "stream": stream_id, "input": "cue", "value": { "track": "drop" } }),
    )
    .await;
    assert_eq!(
        refused_result["structuredContent"]["refusal"]["not_running_because"], "preparing",
        "{refused_result}"
    );
    let refusal = tool_answer(&refused_result)
        .expect_err("an Input whose node does not run reaches no reader");
    assert!(
        refusal.starts_with("write_graph_input failed:") && refusal.contains("not running"),
        "{refusal}"
    );
    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "write_graph_input",
            json!({ "stream": stream_id, "input": "fader", "value": 1 }),
        )
        .await,
    )
    .expect_err("an Input no GraphInput declares is refused");
    assert!(
        refusal.contains("`fader`") && refusal.contains("`cue`"),
        "the refusal names the Inputs the stream has: {refusal}"
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// A `GraphInput` whose `default` its family does not take is refused at load, naming the node.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_input_default_its_family_does_not_take_is_refused_at_load() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_whose_cue_input_two_nodes_read(json!("intro")),
    );

    let refusal = engine
        .a_stream_refused_loading_from_its_graph_document(&project, "main")
        .to_string();

    assert!(
        refusal.contains("`cue`")
            && refusal.contains("`default`")
            && refusal.contains("JSON object"),
        "{refusal}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_listing_the_interpreter_does_not_print_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_project_node_types",
            json!({ "project_directory": cell.path() }),
        )
        .await,
    )
    .expect_err("an interpreter that prints nothing lists nothing");

    assert!(
        refusal.contains("list_project_node_types failed")
            && refusal.contains("printed no listing"),
        "{refusal}"
    );
}

/// The graph of the stream `stream_name` holding one fragment named after each stream of
/// `held_stream_names`, each a reference to `project`'s own stream.
fn the_graph_of_a_stream_holding_fragments_of(
    project: &ProjectWithGraphDocuments,
    stream_name: &str,
    held_stream_names: &[&str],
) -> Value {
    let fragment_nodes: Vec<Value> = held_stream_names
        .iter()
        .map(|held_stream_name| {
            json!({
                "id": Uuid::new_v4(),
                "name": held_stream_name,
                "type": STREAM_FRAGMENT_NODE_TYPE,
                "config": { "stream": { "asset": project.stream_asset_of(held_stream_name) } },
            })
        })
        .collect();
    json!({ "stream": stream_name, "nodes": fragment_nodes, "links": [] })
}

/// `left` holds `main` and `top` holds `left`, all three loaded. An edit to `main` reaches the
/// fragment of it in `left` and, through `left`, in `top`, while their graph documents keep the
/// references alone; its undo reaches both again.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_to_a_stream_reaches_each_loaded_stream_holding_it_at_any_depth() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = a_project_whose_main_stream_holds_one_source();
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "left",
        &["main"],
    ));
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "top",
        &["left"],
    ));
    engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let main_asset = cell.stream_asset_of("main");
    let left_id = engine.a_stream_loaded_from_its_graph_document(&cell, "left");
    let top_id = engine.a_stream_loaded_from_its_graph_document(&cell, "top");
    let main_in_top = |engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory| {
        let left_in_top = the_node_named(&engine.the_live_graph_of(&top_id), "left");
        the_node_names_a_fragment_places(&the_node_named(&left_in_top["fragment"], "main"))
    };
    assert_eq!(main_in_top(&engine), ["source"]);

    let sink_type = StreamToolsTestSink::processor_class_import_path();
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "add_node",
            json!({ "stream": main_asset, "type": sink_type.as_str(), "name": "wrist",
                    "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("main is edited");

    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&left_id),
            "main"
        )),
        ["source", "wrist"]
    );
    assert_eq!(main_in_top(&engine), ["source", "wrist"]);
    assert!(
        the_node_named(&cell.the_graph_document_of("left"), "main")
            .get("fragment")
            .is_none()
    );

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": cell.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the edit is undone");
    assert_eq!(main_in_top(&engine), ["source"]);
}

/// `left` holds `main`; placing `left` in `main` would make each hold the other, so the edit is
/// refused naming both streams, and `main` is left as it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_fragment_that_would_close_a_cycle_is_refused_naming_each_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = a_project_whose_main_stream_holds_one_source();
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "left",
        &["main"],
    ));
    let main_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let main_asset = cell.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "add_node",
            json!({ "stream": main_asset, "type": STREAM_FRAGMENT_NODE_TYPE, "name": "left",
                    "config": { "stream": { "asset": cell.stream_asset_of("left") } },
                    "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect_err("a fragment closing a cycle is refused");

    assert!(
        refusal.contains("cycle") && refusal.contains("`main`") && refusal.contains("`left`"),
        "{refusal}"
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&main_id)),
        ["source"]
    );
}

/// An open project runs `look` as its own stream until an edit places it in `main`, when it runs
/// only there; undoing the placement runs `look` as its own stream again.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_an_edit_places_runs_only_where_it_is_placed_until_the_edit_is_undone() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_project_whose_main_stream_holds_one_source();
    let mut look = the_graph_of_a_stream_named("look");
    look["nodes"][0]["id"] = json!(Uuid::new_v4());
    project.write_the_graph_document(look);
    let look_id = project.stream_id_of("look");
    let main_id = project.stream_id_of("main");
    let opened =
        the_run_of_the_project_once_finished(operations_on_the_loaded_streams, project.path())
            .await;
    assert!(
        operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .contains(&look_id),
        "look runs as its own stream while nothing places it: {opened}"
    );

    tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "add_node",
            json!({ "stream": project.stream_asset_of("main"), "type": STREAM_FRAGMENT_NODE_TYPE,
                    "name": "look",
                    "config": { "stream": { "asset": project.stream_asset_of("look") } },
                    "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("main places look");
    let loaded_once_placed = operations_on_the_loaded_streams.ids_of_the_loaded_streams();
    assert!(
        !loaded_once_placed.contains(&look_id) && loaded_once_placed.contains(&main_id),
        "{loaded_once_placed:?}"
    );
    assert!(
        !engine
            .stream_records()
            .read_every()
            .into_iter()
            .flatten()
            .any(|record| record.stream_id() == look_id),
        "look is no stream of its own once main places it"
    );

    tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    for action_under_way in operations_on_the_loaded_streams.stream_actions_under_way() {
        the_stream_action_once_finished(
            operations_on_the_loaded_streams,
            &action_under_way.action_id,
        )
        .await;
    }
    assert!(
        operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .contains(&look_id),
        "look runs as its own stream again once nothing places it"
    );
}

/// Each file of `directory` outside its `.venv`, zipped into `archive` under `top_folder/`, as a
/// GitHub archive wraps a repository.
fn zip_the_project_under_a_top_folder(directory: &Path, archive: &Path, top_folder: &str) {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::fs::File::create(archive).expect("the archive"));
    let mut directories = vec![directory.to_path_buf()];
    while let Some(reading) = directories.pop() {
        for entry in std::fs::read_dir(&reading).expect("the folder reads") {
            let entry = entry.expect("the entry reads");
            if entry.file_name() == ".venv" {
                continue;
            }
            if entry.file_type().expect("the entry's type").is_dir() {
                directories.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(directory)
                .expect("the entry is in the project")
                .to_string_lossy()
                .into_owned();
            writer
                .start_file(
                    format!("{top_folder}/{relative}"),
                    zip::write::SimpleFileOptions::default(),
                )
                .expect("the entry starts");
            writer
                .write_all(&std::fs::read(entry.path()).expect("the file reads"))
                .expect("the entry is written");
        }
    }
    writer.finish().expect("the archive finishes");
}

/// A project zipped as a GitHub archive, unpacked by `import_project` into `project_directory`,
/// with the stub venv a run finds there: what it answered.
async fn a_project_imported_from_a_zip_into(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    zipped: &ProjectWithGraphDocuments,
    project_directory: &Path,
) -> Value {
    let archive = project_directory.with_extension("zip");
    zip_the_project_under_a_top_folder(zipped.path(), &archive, "cell-main");
    let imported = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "import_project",
            json!({ "source": archive, "project_directory": project_directory }),
        )
        .await,
    )
    .expect("the zip is imported");
    let interpreter = project_directory.join(".venv/bin/python");
    std::fs::create_dir_all(interpreter.parent().expect("the venv's bin"))
        .expect("the venv's bin directory");
    write_an_executable_script_from_a_child_process(
        &interpreter,
        STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
    );
    imported
}

/// The graph of the stream `main` holding one node with its UUID, as the runtime writes it.
fn the_graph_of_main_with_a_node_uuid() -> Value {
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"][0]["id"] = json!(Uuid::new_v4());
    graph
}

/// A project unpacked by `import_project` loads its stream under the new project's UUID and
/// folder, never the zipped project's, so the same zip imported twice is two streams.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_imported_from_a_zip_of_one_loads_its_stream_as_a_project_of_its_own() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let imported_under = tempfile::tempdir().expect("a folder to import into");
    let imported_under = std::fs::canonicalize(imported_under.path()).expect("the folder resolves");
    let project_directory = imported_under.join("cell");

    let imported = a_project_imported_from_a_zip_into(
        operations_on_the_loaded_streams,
        &zipped,
        &project_directory,
    )
    .await;
    let finished =
        the_run_of_the_project_once_finished(operations_on_the_loaded_streams, &project_directory)
            .await;

    assert_eq!(imported["project_directory"], json!(project_directory));
    assert_eq!(imported["name"], TEST_PROJECT_NAME);
    assert_ne!(imported["project"], json!(zipped.project.to_string()));
    assert!(
        imported["commit"]
            .as_str()
            .is_some_and(|commit| !commit.is_empty())
    );
    let stream_id = the_one_stream_of(&finished)["stream"]
        .as_str()
        .expect("the stream's id")
        .to_owned();
    assert_ne!(stream_id, zipped.stream_id_of("main"));
    let listing = the_listing_of(operations_on_the_loaded_streams, &stream_id).await;
    assert_eq!(
        listing["location"],
        json!({
            "project_directory": project_directory,
            "project": imported["project"],
            "stream_asset": zipped.stream_asset_of("main"),
        }),
        "{listing}"
    );
}

/// A project unpacked by `import_project` runs its stream.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_project_imported_from_a_zip_of_one_runs_its_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let imported_under = tempfile::tempdir().expect("a folder to import into");
    let imported_under = std::fs::canonicalize(imported_under.path()).expect("the folder resolves");
    let project_directory = imported_under.join("cell");

    a_project_imported_from_a_zip_into(
        operations_on_the_loaded_streams,
        &zipped,
        &project_directory,
    )
    .await;
    the_run_of_the_project_once_finished(operations_on_the_loaded_streams, &project_directory)
        .await;
    let finished = the_project_action_once_finished(
        "start_playback",
        operations_on_the_loaded_streams,
        &project_directory,
    )
    .await;

    assert_eq!(finished["state"], "done", "{finished}");
    assert_eq!(
        the_one_stream_of(&finished)["state"],
        "running",
        "{finished}"
    );
}

/// `import_project` into a folder that already exists is refused in the runtime's own words.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_imported_into_a_folder_that_exists_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let unpacked_under = tempfile::tempdir().expect("a folder to import into");
    let archive = unpacked_under.path().join("cell-main.zip");
    zip_the_project_under_a_top_folder(zipped.path(), &archive, "cell-main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_project",
            json!({ "source": archive, "project_directory": unpacked_under.path() }),
        )
        .await,
    )
    .expect_err("an existing folder is refused");

    assert!(
        refusal.contains("import_project failed") && refusal.contains("already exists"),
        "{refusal}"
    );
}

/// A run whose venv the runtime's uv makes is followed live through the resource of the actions
/// under way: a listen on it is told as uv starts each download, the resource then holds each
/// package uv is fetching with its size, and once the sync ends the action's own resource holds
/// what uv installed, each package at its version.
#[tokio::test(flavor = "multi_thread")]
async fn a_venv_sync_is_followed_package_by_package_through_the_stream_action_resources() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let downloads_released = uv_directory.path().join("released");
    let uv_writing_what_uv_writes = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_writing_what_uv_writes,
        &format!(
            "#!/bin/sh
printf 'Using CPython 3.13.18 interpreter at: /usr/local/bin/python3\\nCreating virtual environment at: .venv\\nResolved 8 packages in 776ms\\nDownloading numpy (15.9MiB)\\nDownloading pillow (6.8MiB)\\n' >&2
while [ ! -e '{released}' ]; do sleep 0.05; done
printf ' Downloaded pillow\\n Downloaded numpy\\nPrepared 4 packages in 12.50s\\nInstalled 4 packages in 8ms\\n + numpy==2.5.4\\n + packaging==26.3\\n + pillow==13.0.0\\n + six==1.17.0\\n' >&2
mkdir -p .venv/bin && printf '#!/bin/sh\\nexit 0\\n' > .venv/bin/python && chmod 755 .venv/bin/python
",
            released = downloads_released.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_writing_what_uv_writes)
        .expect("the runtime is handed its uv");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let following_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let read_the_resource = |uri: String| {
        let following_connection = &following_connection;
        async move {
            let read = following_connection
                .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
                .await
                .expect("the resource reads");
            let read = serde_json::to_value(read).unwrap();
            serde_json::from_str::<Value>(read["contents"][0]["text"].as_str().unwrap()).unwrap()
        }
    };
    let mut subscription = following_connection
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription("streamlib://stream-actions".to_owned())
                .build(),
        )
        .await
        .expect("the runtime acknowledges a listen on the actions under way");

    let queued = tool_answer(
        &tool_call_over_the_connection(
            &following_connection,
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let downloading = loop {
        let update = tokio::time::timeout_at(deadline.into(), subscription.next())
            .await
            .expect("each change of the action is announced")
            .expect("the listen is still open")
            .expect("the update is well formed");
        let rmcp::model::ServerNotification::ResourceUpdatedNotification(update) = update else {
            panic!("expected notifications/resources/updated, got {update:?}");
        };
        assert_eq!(update.params.uri, "streamlib://stream-actions");
        let under_way = read_the_resource("streamlib://stream-actions".to_owned()).await;
        let the_action = &under_way["actions"][0];
        if the_action["venv_sync"]["downloads_started"] == 2 {
            break the_action.clone();
        }
    };
    assert_eq!(downloading["action"], action.as_str());
    assert_eq!(downloading["step"], "making_the_venv");
    assert_eq!(
        downloading["venv_sync"],
        json!({
            "phase": "downloading",
            "packages_resolved": 8,
            "packages_prepared": null,
            "packages_installed": null,
            "downloads_started": 2,
            "downloads_done": 0,
            "packages": [
                {
                    "name": "numpy", "version": null, "kind": "distribution",
                    "state": "downloading", "download_size_bytes": 16_672_358,
                },
                {
                    "name": "pillow", "version": null, "kind": "distribution",
                    "state": "downloading", "download_size_bytes": 7_130_317,
                },
            ],
        })
    );

    std::fs::write(&downloads_released, "").expect("the downloads are released");
    the_stream_action_once_finished(&engine.operations_on_the_loaded_streams(), &action).await;
    let finished = read_the_resource(format!("streamlib://stream-action/{action}")).await;
    let venv_sync = &finished["venv_sync"];
    assert_eq!(
        (
            &venv_sync["phase"],
            &venv_sync["packages_prepared"],
            &venv_sync["packages_installed"],
            &venv_sync["downloads_done"],
        ),
        (&json!("installed"), &json!(4), &json!(4), &json!(2)),
        "{finished}"
    );
    let installed: Vec<(&str, &str, &str)> = venv_sync["packages"]
        .as_array()
        .expect("the packages uv named")
        .iter()
        .map(|package| {
            (
                package["name"].as_str().unwrap(),
                package["version"].as_str().unwrap(),
                package["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        installed,
        [
            ("numpy", "2.5.4", "installed"),
            ("pillow", "13.0.0", "installed"),
            ("packaging", "26.3", "installed"),
            ("six", "1.17.0", "installed"),
        ]
    );
    assert_eq!(
        read_the_resource("streamlib://stream-actions".to_owned()).await,
        json!({ "actions": [] })
    );
}
