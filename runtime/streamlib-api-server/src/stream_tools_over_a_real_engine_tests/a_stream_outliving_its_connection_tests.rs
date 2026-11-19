// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream run over a `/mcp/stdio` connection outlives the connection and the local API, against
//! an engine whose run loads an empty stream without starting it.

use super::*;

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
    ) -> Result<Vec<NodeDescriptor>> {
        self.engine
            .node_catalog_of_the_stream_a_call_names(stream_id)
    }
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<NodeDescriptor>> {
        self.engine
            .node_types_described_in_the_interpreter_of_the_stream_a_call_names(stream_id)
    }
    fn node_types_installed_modules_declare_in_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<
        Vec<(
            NodeDescriptor,
            streamlib::sdk::nodes::InstalledModuleOfANodeType,
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
    fn make_the_projects_venv_current(
        &self,
        project_directory: &Path,
    ) -> Result<StreamActionQueued> {
        OperationsOnTheStreamsLoadedInThisRuntime::make_the_projects_venv_current(
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
    fn the_streams_to_read_or_edit(
        &self,
        stream_ids: &[String],
    ) -> Result<streamlib::sdk::runtime::StreamsToReadOrEdit<'_>> {
        OperationsOnTheStreamsLoadedInThisRuntime::the_streams_to_read_or_edit(
            self.engine.as_ref(),
            stream_ids,
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
    fn the_core_nodes_project_directory_this_runtime_carries(&self) -> Option<PathBuf> {
        OperationsOnTheStreamsLoadedInThisRuntime::the_core_nodes_project_directory_this_runtime_carries(
            self.engine.as_ref(),
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
    fn check_a_projects_extension_code(
        &self,
        project_directory: &Path,
        paths: Option<&[String]>,
    ) -> streamlib::sdk::runtime::ExtensionCodeCheck {
        OperationsOnTheStreamsLoadedInThisRuntime::check_a_projects_extension_code(
            self.engine.as_ref(),
            project_directory,
            paths,
        )
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
