// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
use streamlib::sdk::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PinDeclaration, PinType, PortDescriptor,
};
use streamlib::sdk::error::Result;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, OptionsForLoadingOneStream,
    ProjectOpenOrReloadRequest, Runner, RuntimeOperations, StreamActionQueued, StreamEnvironment,
    StreamListing, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordsInTheStateDirectory,
};
use streamlib_control_contract::PROJECT_LOG_RECORDS_PER_NOTIFICATION;
use streamlib_project_repository::{GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE};
use uuid::Uuid;

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{first_text_block_json, tool_call_result};
use crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream;

mod a_stream_outliving_its_connection_tests;
mod an_edit_a_placement_refuses_tests;
mod an_edit_recorded_before_the_update_tests;
mod built_in_value_input_set_while_playing_tests;
mod bundle_frame_link_tests;
mod bundle_member_link_tests;
mod bundle_type_offered_by_its_stream_tests;
mod canvas_group_and_note_tests;
mod collapsed_selection_and_placed_stream_tests;
mod connection_wire_tests;
mod constant_typed_by_its_pin_type_tests;
mod convert_node_inserted_on_a_link_tests;
mod convert_node_inside_a_placed_stream_tests;
mod graph_action_feed_and_subscription_tests;
mod graph_answer_read_through_the_contract_tests;
mod graph_edit_transaction_and_undo_tests;
mod graph_edits_of_a_playing_stream_tests;
mod graph_generator_fragment_tests;
mod graph_input_retype_worked_out_before_the_edit_tests;
mod graph_input_retyped_through_transact_tests;
mod imported_and_nested_stream_fragment_tests;
mod input_value_lifetime_tests;
mod input_value_set_live_tests;
mod input_value_set_tests;
mod input_value_test_node;
mod link_candidates_checked_before_one_is_made_tests;
mod link_checked_before_it_is_made_tests;
mod live_input_change_restarting_nothing_tests;
mod loop_closing_link_judged_before_commit_tests;
mod node_counting_its_builds;
mod node_rebuilt_by_a_setting_tests;
mod node_type_group_tests;
mod node_type_reads_tests;
mod node_types_of_the_graph_itself_tests;
mod pin_group_count_set_as_an_input_value_tests;
mod pin_group_gap_closed_by_a_disconnect_tests;
mod pin_retyped_by_its_input_tests;
mod played_project_end_to_end_tests;
mod project_conversion_named_on_a_link_tests;
mod project_create_open_and_cancel_tests;
mod project_log_tests;
mod project_node_type_listing_tests;
mod project_view_and_graph_input_tests;
mod project_zip_import_tests;
mod reactive_reader_rule_of_a_transaction_tests;
mod renamed_pin_tests;
mod stream_input_of_a_playing_stream_tests;
mod stream_listing_actions_and_record_tests;
mod stream_named_by_its_id_tests;
mod stub_exclusive_device;
mod tensor_to_texture_input_set_while_playing_tests;
mod texture_format_convert_inserted_on_a_link_tests;
mod trigger_fire_tests;
mod typed_pin_link_tests;
mod value_input_linked_after_a_live_add_tests;
mod venv_sync_stream_action_resource_tests;
mod view_output_of_a_playing_stream_tests;

/// A native source with one output port, `video`, and nothing to do.
#[streamlib::sdk::node(execution = manual, output("video", pin_type = Texture, formats = ["rgba8_unorm"]))]
pub(crate) fn stream_tools_test_source() {}

fn register_the_stream_tools_test_source_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestSource::Node>());
}

/// The graph of a stream named `stream_name`: one [`StreamToolsTestSource`] named `source`.
fn the_graph_of_a_stream_named(stream_name: &str) -> Value {
    json!({
        "stream": stream_name,
        "nodes": [{
            "name": "source",
            "type": StreamToolsTestSource::node_class_import_path().as_str(),
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
            .set_node_interpreter_lend_directory(PathBuf::from("/opt/tatolab/lib/tatolab/lend"))
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
}

/// The JSON a tool's successful result states, or the text of its refusal.
fn tool_answer(tool_result: &Value) -> std::result::Result<Value, String> {
    if tool_result["isError"] == true {
        return Err(crate::mcp::tests::the_refusal_of_a_tool_result(tool_result).runtime_words);
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

/// The one stream a finished stream action took in.
fn the_one_stream_of(action: &Value) -> &Value {
    match action["streams"].as_array().map(Vec::as_slice) {
        Some([stream]) => stream,
        _ => panic!("the action took in one stream: {action}"),
    }
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

/// A native sink with one input port, `video`, and nothing to do.
#[streamlib::sdk::node(
    execution = manual,
    input("video", delivery_profile = "newest", pin_type = Texture)
)]
pub(crate) fn stream_tools_test_sink() {}

fn register_the_stream_tools_test_sink_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestSink::Node>());
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

    /// The graph document of this project's stream `stream_name` as its project reads it to
    /// edit it, each link onto a placed stream naming the port by the name it is declared under.
    fn the_graph_document_as_its_project_reads_it(&self, stream_name: &str) -> Value {
        streamlib_project_repository::ProjectRepository::open(self.path())
            .expect("the project opens")
            .stream_graph_document(
                self.the_graph_document_of(stream_name)["asset"]
                    .as_str()
                    .and_then(|asset| Uuid::parse_str(asset).ok())
                    .expect("the graph document names its stream asset"),
            )
            .expect("the graph document reads")
            .to_value()
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

    /// Load `project`'s stream `stream_name` from its graph document on disk, start it and wait
    /// until each of its nodes runs; returns its id.
    fn a_stream_started_from_its_graph_document(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> String {
        let stream_id = self.a_stream_loaded_from_its_graph_document(project, stream_name);
        let stream = self
            .engine
            .loaded_stream_of_the_id(&stream_id)
            .expect("the stream is loaded");
        stream.start().expect("the stream starts");
        stream
            .wait_until_every_node_is_running(Duration::from_secs(30))
            .expect("every node runs");
        stream_id
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
                    streamlib_project_repository::graph_document_as_its_project_reads_it(
                        project.path(),
                        &project.the_graph_document_of(stream_name),
                        &streamlib_project_repository::StreamDocumentsAnEditStaged::default(),
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

    /// The live graph of the loaded stream `stream_id`.
    fn the_live_graph_of(&self, stream_id: &str) -> Value {
        self.engine
            .runtime_operations_of_the_stream_a_call_names(stream_id)
            .expect("the stream is loaded")
            .to_json()
            .expect("the graph renders")
    }

    /// `project` opened and its stream `main` started, served over a fresh socket; answers the
    /// stream id and the control plane serving it.
    async fn the_stream_main_of_the_project_started_and_served(
        &self,
        project: &ProjectWithGraphDocuments,
    ) -> (
        String,
        crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient,
    ) {
        let operations_on_the_loaded_streams = self.operations_on_the_loaded_streams();
        the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path())
            .await;
        let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
            Arc::clone(&operations_on_the_loaded_streams),
        )
        .await;
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
        let started =
            the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
        assert_eq!(started["state"], "done", "{started}");
        (project.stream_id_of("main"), served)
    }
}

/// Each node's name in `graph`, sorted, leaving out a node a stream not started holds
/// pending its deletion at the start.
fn sorted_node_names_in(graph: &Value) -> Vec<String> {
    let mut names: Vec<String> = the_entries_held_in(graph, "nodes")
        .filter_map(|node| node["name"].as_str().map(str::to_owned))
        .collect();
    names.sort_unstable();
    names
}

/// The entries of `graph`'s `key`, `nodes` or `links`, leaving out one pending its deletion.
fn the_entries_held_in<'graph>(
    graph: &'graph Value,
    key: &str,
) -> impl Iterator<Item = &'graph Value> {
    graph[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["components"]["pending_deletion"] != true)
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
    the_entries_held_in(graph, "links")
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

/// A conversion a refused link names to place between its two ends: its node type, and the input
/// and output the link would run through.
#[derive(Clone, Copy)]
struct ConversionToPlace<'names> {
    node_type: &'names str,
    input_port: &'names str,
    output_port: &'names str,
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The params of the `transact` placing `conversion` between the two ends of `link`, in the
    /// loaded stream `stream_id`: `connect` refuses `link` with `link_needs_a_module_conversion`,
    /// naming `conversion`, and suggests that call, and `check_link` answers the same refusal and
    /// call. Each end of `link` is a node's UUID.
    async fn the_call_placing_the_conversion_a_refused_link_names(
        &self,
        stream_id: &str,
        link: &Value,
        conversion: ConversionToPlace<'_>,
    ) -> Value {
        use streamlib_runtime_client_contract::local_api_wire_contract::{
            ControlOperationRefusalCode, SuggestedOperationCall,
        };
        let mut connect = link.clone();
        connect["author"] = an_agent_named("alice");
        let refusal = self
            .the_answer_or_the_refusal_of("connect", connect)
            .await
            .expect_err("connect never places a conversion a module or a project states");

        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::LinkNeedsAModuleConversion,
            "{refusal:?}"
        );
        assert!(
            refusal
                .runtime_words
                .contains(&format!("`{}`", conversion.node_type)),
            "{refusal:?}"
        );
        let Some(SuggestedOperationCall { operation, params }) = refusal.suggested_call.clone()
        else {
            panic!("the refusal suggests the call that places the conversion: {refusal:?}");
        };
        assert_eq!(operation, "transact");
        let params = Value::Object(params);
        assert_eq!(
            params,
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_id,
                  "type": conversion.node_type, "as": "conversion" },
                { "operation": "connect", "stream": stream_id,
                  "source": link["source"],
                  "target": { "node": "conversion", "port": conversion.input_port } },
                { "operation": "connect", "stream": stream_id,
                  "source": { "node": "conversion", "port": conversion.output_port },
                  "target": link["target"] },
            ]})
        );

        let checked = self
            .the_answer_or_the_refusal_of("check_link", link.clone())
            .await
            .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));
        assert_eq!(checked["outcome"], "refused", "{checked}");
        assert_eq!(
            checked["refusal"]["refusal_code"], "link_needs_a_module_conversion",
            "{checked}"
        );
        assert_eq!(
            checked["refusal"]["suggested_call"],
            serde_json::to_value(&refusal.suggested_call).unwrap(),
            "check_link suggests the call connect suggests"
        );
        params
    }
}

/// The port `name`, typed `pin_type`.
fn a_typed_port(name: &str, pin_type: PinType) -> PortDescriptor {
    PortDescriptor::new(name, "", true, PinDeclaration::exactly(pin_type))
        .with_delivery_profile("newest")
}

/// The Python function node type `node_type` taking `inputs` and giving `outputs`.
fn a_python_function_node_type(
    node_type: &str,
    inputs: Vec<PortDescriptor>,
    outputs: Vec<PortDescriptor>,
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a Python function node type a test registers",
    );
    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Python,
    };
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
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

/// The graph of the stream `main` holding one node with its UUID, as the runtime writes it.
fn the_graph_of_main_with_a_node_uuid() -> Value {
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"][0]["id"] = json!(Uuid::new_v4());
    graph
}

/// The mixer of a project's `main`, collapsed into the placed stream `sub`, beside a
/// `GraphInput` `volume` added inside `sub`: the placing node's UUID, and the mixer's and
/// `volume`'s UUIDs as `main` draws them inside it.
struct TheMixerInsideAPlacedStream {
    subgraph: String,
    mixer: String,
    volume: String,
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The mixer of `project`'s loaded `main` collapsed by `alice` into the placed stream
    /// `sub`, with a `GraphInput` `volume` added beside it inside `sub`.
    async fn the_mixer_collapsed_into_a_placed_stream(
        &self,
        project: &ProjectWithGraphDocuments,
    ) -> TheMixerInsideAPlacedStream {
        let stream_asset = project.stream_asset_of("main");
        let collapsed = self
            .the_answer_or_the_refusal_of(
                "collapse_selection_into_subgraph",
                json!({ "stream": stream_asset, "nodes": [input_value_set_tests::MIXER_NODE_UUID], "name": "sub",
                        "author": an_agent_named("alice") }),
            )
            .await
            .expect("the mixer collapses into `sub`");
        let subgraph = collapsed["added_nodes"][0]["node"]
            .as_str()
            .expect("the placing node")
            .to_owned();
        let set_on_volume = |input: &str, value: Value| {
            json!({ "operation": "set_node_input_value", "stream": stream_asset,
                    "level": [subgraph], "node": "volume", "input": input, "value": value })
        };
        self.the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "level": [subgraph],
                  "type": streamlib_project_repository::GRAPH_INPUT_NODE_TYPE,
                  "name": "volume", "as": "volume" },
                set_on_volume("port", json!("volume")),
                set_on_volume("pin_type", json!({ "type": "float" })),
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("a `GraphInput` is added inside `sub`");
        let sub = project.the_graph_document_of("sub");
        let drawn = |name: &str| {
            the_uuid_a_held_node_takes_under(
                &subgraph,
                the_node_named(&sub, name)["id"]
                    .as_str()
                    .expect("the node's UUID"),
            )
        };
        TheMixerInsideAPlacedStream {
            mixer: drawn("mixer"),
            volume: drawn("volume"),
            subgraph,
        }
    }
}

/// Assert `graph_document`, a stream's graph document, holds what `running`, its live graph
/// render, runs: the same nodes by UUID, each with its name, type and saved input values, the
/// same links, and no input running a live value the document does not hold.
fn assert_the_graph_document_equals_the_running_graph(graph_document: &Value, running: &Value) {
    let live_inputs: Vec<String> = the_entries_held_in(running, "nodes")
        .flat_map(|node| {
            node["ports"]["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|input| {
                    std::iter::once(input).chain(input["members"].as_array().into_iter().flatten())
                })
                .filter(|input| input["value_comes_from"] == "live")
                .map(move |input| format!("{}: {input}", node["name"]))
        })
        .collect();
    assert_eq!(
        live_inputs,
        Vec::<String>::new(),
        "inputs run live values the document does not hold"
    );
    assert_eq!(
        the_nodes_and_links_held_in(graph_document),
        the_nodes_and_links_held_in(running),
        "the graph document and the running graph"
    );
}

/// The nodes `graph` holds, by UUID, each with its name, type and saved input values, and its
/// links, each end by its node's UUID; `graph` is a graph document or a live graph render, a
/// document's link naming its node by UUID or by name.
fn the_nodes_and_links_held_in(graph: &Value) -> Value {
    let nodes: Vec<&Value> = the_entries_held_in(graph, "nodes").collect();
    let uuid_of = |node: &Value| -> Value {
        nodes
            .iter()
            .find(|held| held["id"] == *node || held["name"] == *node)
            .map(|held| held["id"].clone())
            .unwrap_or_else(|| panic!("a link names a node `{node}` the graph does not hold"))
    };
    let mut held_nodes: Vec<Value> = nodes
        .iter()
        .map(|node| {
            let input_values = match &node["input_values"] {
                Value::Null => json!({}),
                input_values => input_values.clone(),
            };
            json!({ "id": node["id"], "name": node["name"], "type": node["type"],
                    "input_values": input_values })
        })
        .collect();
    held_nodes.sort_by_cached_key(|node| node["id"].to_string());
    let end_of = |end: &Value| json!({ "node": uuid_of(&end["node"]), "port": end["port"], "member": end["member"] });
    let mut held_links: Vec<Value> = the_entries_held_in(graph, "links")
        .map(|link| json!({ "source": end_of(&link["source"]), "target": end_of(&link["target"]) }))
        .collect();
    held_links.sort_by_cached_key(Value::to_string);
    json!({ "nodes": held_nodes, "links": held_links })
}
