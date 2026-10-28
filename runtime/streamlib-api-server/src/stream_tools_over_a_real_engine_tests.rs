// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream tools over a real engine: a stream run over a `/mcp/stdio`
//! connection outlives the connection and the local API, `logs` pages a
//! stream's records by sequence number, and `list_streams`,
//! `stop_stream`, `start_stream`, `remove_stream`, `rename_stream` and
//! `expose_port` act by stream id on an engine recording its streams in a temporary state
//! directory.
//!
//! A stream's start creates the engine's GPU context, so the GPU-free tests
//! load without starting — through the engine's own loads, or a runtime whose
//! `run_stream` loads an empty stream — and the tests that run a project's
//! stream function end to end are gated on `hardware-tests`.

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
use streamlib::sdk::graph::OutputPortExposureLevel;
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::{
    LogRecordsPageOfOneLoadedStream, OperationsOnTheStreamsLoadedInThisRuntime,
    OptionsForLoadingOneStream, OutputPortExposureOutcome, RunStreamRequest, Runner,
    RuntimeOperations, StreamEnvironment, StreamListing, StreamLocation, StreamRecord,
    StreamRecordAsRead, StreamRecordsInTheStateDirectory, StreamRemoveOutcome, StreamRenameOutcome,
    StreamRunOutcome, StreamStartOutcome, StreamStopOutcome,
};

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{first_text_block_json, tool_call_result};
use crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream;

/// How long a test waits for what an engine does on another thread — a record
/// reaching a stream's log.
const WHAT_THE_ENGINE_DOES_ON_ANOTHER_THREAD_LANDS_WITHIN: Duration = Duration::from_secs(10);

/// A native source with one output port, `video`, and nothing to do.
#[streamlib::sdk::processor(execution = manual, output("video"))]
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

/// The graph a stream function named `stream_id` compiles to: one
/// [`StreamToolsTestSource`] named `source`, exposing nothing.
fn the_graph_of_a_function_named(stream_id: &str) -> Value {
    json!({
        "stream": stream_id,
        "nodes": [{
            "name": "source",
            "type": StreamToolsTestSource::processor_class_import_path().as_str(),
            "config": {},
        }],
        "links": [],
        "exposed": [],
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

/// A project whose `.venv/bin/python` prints `graph` as its compile.
struct ProjectWhosePythonPrintsAGraph {
    project_directory: tempfile::TempDir,
}

impl ProjectWhosePythonPrintsAGraph {
    fn compiling(graph: Value) -> Self {
        Self::compiling_warning(graph, &[])
    }

    /// A project whose compile writes each of `warnings` on a line of its
    /// standard error, then prints `graph`.
    fn compiling_warning(graph: Value, warnings: &[&str]) -> Self {
        let project = Self {
            project_directory: tempfile::tempdir().expect("a project directory"),
        };
        std::fs::create_dir_all(project.path().join(".venv").join("bin"))
            .expect("the venv's bin directory");
        project.compile_to_warning(graph, warnings);
        project
    }

    /// From now on, the compile writes each of `warnings` on a line of its
    /// standard error, then prints `graph` as the function `stream:<graph's
    /// stream>` of this project.
    fn compile_to_warning(&self, graph: Value, warnings: &[&str]) {
        let function = graph["stream"]
            .as_str()
            .expect("the graph names its function")
            .to_string();
        let compile_document = json!({
            "stream_graph": graph,
            "project_directory": self.path(),
            "stream_function": self.location_of(&function).stream_function,
        });
        let warnings_written: String = warnings
            .iter()
            .map(|warning| format!("echo '{warning}' >&2\n"))
            .collect();
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            &format!(
                "#!/bin/sh\n{warnings_written}cat <<'COMPILED'\n{compile_document}\nCOMPILED\n"
            ),
        );
    }

    /// From now on, the compile exits 1 printing `traceback`.
    fn fail_to_compile_printing(&self, traceback: &str) {
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            &format!("#!/bin/sh\necho \"{traceback}\" >&2\nexit 1\n"),
        );
    }

    fn path(&self) -> &Path {
        self.project_directory.path()
    }

    fn interpreter(&self) -> PathBuf {
        self.path().join(".venv").join("bin").join("python")
    }

    /// The location of this project's stream function `function`, in `stream.py`.
    fn location_of(&self, function: &str) -> StreamLocation {
        StreamLocation {
            project_directory: self.path().to_path_buf(),
            stream_function: format!("stream:{function}"),
        }
    }

    /// The stream id the location of this project's `function` computes to.
    fn stream_id_of(&self, function: &str) -> String {
        self.location_of(function).stream_id()
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

    /// Load `project`'s function `function` as its library host would, not
    /// started, under the id its location computes to, which it returns.
    fn a_stream_its_host_loaded_without_its_start(
        &self,
        project: &ProjectWhosePythonPrintsAGraph,
        function: &str,
    ) -> String {
        let stream_id = project.stream_id_of(function);
        self.engine
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(the_graph_of_a_function_named(function))
                    .expect("the graph parses"),
                OptionsForLoadingOneStream::in_stream_environment(project.stream_environment())
                    .with_display_name(function)
                    .at_stream_location(project.location_of(function)),
            )
            .expect("the host loads its stream");
        stream_id
    }

    /// Record `project`'s function `function`, stopped when `stopped` says so,
    /// and not loaded; returns its id.
    fn a_stream_recorded_and_not_loaded(
        &self,
        project: &ProjectWhosePythonPrintsAGraph,
        function: &str,
        stopped: bool,
    ) -> String {
        let mut record = StreamRecord::of_a_running_stream(
            project.location_of(function),
            function,
            &project.stream_environment(),
            the_graph_of_a_function_named(function),
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

/// A cross-floor warning, as a test's compile writes it on its standard error.
const CROSS_FLOOR_WARNING_A_COMPILE_WROTE: &str =
    "tatolab: the cross-floor check found 1 thing binding this app to one floor.";

/// A runtime whose `run_stream` loads an empty stream under the requested
/// display name, standing in for an id, without compiling or starting it — a GPU-free stand-in for the
/// engine's own run, answering [`CROSS_FLOOR_WARNING_A_COMPILE_WROTE`] as its compile's —
/// and hands every other call to the engine.
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
    fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine.ids_of_the_loaded_streams()
    }
    fn every_loaded_stream_and_the_output_ports_it_exposes(
        &self,
    ) -> Vec<streamlib::sdk::runtime::LoadedStreamAndTheOutputPortsItExposes> {
        self.engine
            .every_loaded_stream_and_the_output_ports_it_exposes()
    }
    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LogRecordsPageOfOneLoadedStream> {
        self.engine
            .log_records_of_the_stream_a_call_names(stream_id, after, max_count)
    }
    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        let stream_id = request
            .display_name
            .expect("the test names every stream it runs");
        let stream_location = StreamLocation {
            project_directory: request.project_directory,
            stream_function: format!("stream:{stream_id}"),
        };
        let stream = self.engine.load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(self.project_directory.path())
                .with_stream_id(&stream_id),
        )?;
        Ok(StreamRunOutcome {
            stream_id: stream.stream_id().to_string(),
            display_name: stream.display_name(),
            stream_location,
            stream_tag: stream.stream_tag(),
            node_count: 0,
            replaced_a_recorded_stream: false,
            compile_warnings: vec![CROSS_FLOOR_WARNING_A_COMPILE_WROTE.to_string()],
        })
    }
    fn stop_stream(&self, stream_id: &str) -> Result<StreamStopOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::stop_stream(self.engine.as_ref(), stream_id)
    }
    fn start_stream(&self, stream_id: &str) -> Result<StreamStartOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::start_stream(self.engine.as_ref(), stream_id)
    }
    fn remove_stream(&self, stream_id: &str) -> Result<StreamRemoveOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::remove_stream(self.engine.as_ref(), stream_id)
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
    fn expose_port(
        &self,
        stream_id: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::expose_port(
            self.engine.as_ref(),
            stream_id,
            node,
            port,
            level,
        )
    }
}

fn a_run_request_naming(stream_id: &str) -> Value {
    json!({
        "project_directory": "/home/someone/projects/camera",
        "name": stream_id,
    })
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
        &tool_call_over_the_connection(&client, "run_stream", a_run_request_naming("camera")).await,
    )
    .expect("the run is answered");
    assert_eq!(run["stream"], "camera");
    assert_eq!(run["display_name"], "camera");
    assert_eq!(
        run["location"],
        json!({
            "project_directory": "/home/someone/projects/camera",
            "stream_function": "stream:camera",
        })
    );
    assert_eq!(run.get("kept"), None, "a run answers no `kept`: {run}");
    assert_eq!(
        run["compile_warnings"],
        json!([CROSS_FLOOR_WARNING_A_COMPILE_WROTE]),
        "the run's result carries what its compile wrote to its standard error"
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
        &tool_call_over_the_connection(&client, "run_stream", a_run_request_naming("camera")).await,
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

/// The instance a `run_stream` result and a `logs` page name is the load's:
/// the same for every page of one load, and another once the name is loaded
/// again, whose records are numbered from 1 again.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_and_its_logs_pages_name_the_loads_instance_and_a_re_load_names_another() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    let run_camera_and_read_its_logs = || async {
        let run = tool_answer(
            &tool_call_over_the_connection(&client, "run_stream", a_run_request_naming("camera"))
                .await,
        )
        .expect("the run is answered");
        let page = tool_answer(
            &tool_call_over_the_connection(&client, "logs", json!({ "stream": "camera" })).await,
        )
        .expect("logs answers");
        (run, page)
    };

    let (first_run, first_page) = run_camera_and_read_its_logs().await;
    let first_instance = first_run["stream_instance"]
        .as_str()
        .unwrap_or_else(|| panic!("the run names its instance: {first_run}"))
        .to_string();
    assert_eq!(
        first_instance,
        engine
            .loaded_stream_of_the_id("camera")
            .unwrap()
            .stream_tag()
            .to_string()
    );
    assert_eq!(first_page["stream_instance"], first_instance.as_str());
    tool_answer(
        &tool_call_over_the_connection(&client, "stop_stream", json!({ "stream": "camera" })).await,
    )
    .expect("the stream stops");

    let (re_run, re_loaded_page) = run_camera_and_read_its_logs().await;

    assert_ne!(re_run["stream_instance"], first_instance.as_str());
    assert_eq!(re_loaded_page["stream_instance"], re_run["stream_instance"]);
}

/// `logs` reads a stream's records by sequence number: `after` skips what was
/// read, `count` bounds the page, and `next_after` is where to read on from.
#[tokio::test(flavor = "multi_thread")]
async fn logs_pages_a_streams_records_by_sequence_number() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let engine = Runner::new().expect("the engine builds");
    let stream = engine
        .load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                .with_stream_id("paged"),
        )
        .expect("the stream loads");
    stream.log_route().run_entered(|| {
        for token_index in 0..3 {
            tracing::info!("logs-paging-token-{token_index}");
        }
    });
    let operations_on_the_loaded_streams =
        Arc::clone(&engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let token_sequences = |page: &Value| -> Vec<u64> {
        page["records"]
            .as_array()
            .expect("a records array")
            .iter()
            .filter(|numbered| {
                numbered["record"]["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("logs-paging-token-"))
            })
            .map(|numbered| numbered["sequence"].as_u64().expect("a sequence number"))
            .collect()
    };

    let deadline =
        tokio::time::Instant::now() + WHAT_THE_ENGINE_DOES_ON_ANOTHER_THREAD_LANDS_WITHIN;
    let every_record = loop {
        let every_record = tool_answer(
            &tool_call_result(
                Arc::clone(&operations_on_the_loaded_streams),
                "logs",
                json!({ "stream": "paged" }),
            )
            .await,
        )
        .expect("logs answers");
        if token_sequences(&every_record).len() == 3 {
            break every_record;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the three tokens never reached the stream's log: {every_record}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(every_record["stream"], "paged");
    assert_eq!(every_record["records_no_longer_held"], 0);
    let first_token = token_sequences(&every_record)[0];

    let one_after_the_first_token = tool_answer(
        &tool_call_result(
            Arc::clone(&operations_on_the_loaded_streams),
            "logs",
            json!({ "stream": "paged", "after": first_token, "count": 1 }),
        )
        .await,
    )
    .expect("logs answers");
    let records = one_after_the_first_token["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{one_after_the_first_token}");
    assert_eq!(records[0]["sequence"], first_token + 1);
    assert_eq!(one_after_the_first_token["next_after"], first_token + 1);

    let read_on = tool_answer(
        &tool_call_result(
            Arc::clone(&operations_on_the_loaded_streams),
            "logs",
            json!({ "stream": "paged", "after": one_after_the_first_token["next_after"] }),
        )
        .await,
    )
    .expect("logs answers");
    assert_eq!(
        read_on["records"][0]["sequence"],
        first_token + 2,
        "reading on from `next_after` starts at the next record: {read_on}"
    );

    let past_every_record = tool_answer(
        &tool_call_result(
            Arc::clone(&operations_on_the_loaded_streams),
            "logs",
            json!({ "stream": "paged", "after": u64::MAX }),
        )
        .await,
    )
    .expect("logs answers");
    assert_eq!(past_every_record["records"], json!([]));

    let refusal = tool_answer(
        &tool_call_result(
            operations_on_the_loaded_streams,
            "logs",
            json!({ "stream": "unloaded" }),
        )
        .await,
    )
    .expect_err("a stream not loaded is refused");
    assert!(
        refusal.contains("unloaded") && refusal.contains("paged"),
        "{refusal}"
    );
    engine.unload_stream("paged").expect("the stream unloads");
}

// ============================================================================
// list, stop, start, remove and expose against an engine recording its streams
// ============================================================================

/// `list_streams` lists each stream by its id with its display name and
/// location, and every tool resolves a stream by that id exactly.
#[tokio::test(flavor = "multi_thread")]
async fn list_streams_lists_each_stream_by_id_and_every_tool_resolves_that_id_exactly() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
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

    let location_of = |function: &str| {
        json!({
            "project_directory": project.path(),
            "stream_function": format!("stream:{function}"),
        })
    };
    let mut expected_streams = vec![
        json!({ "stream": live, "display_name": "live", "location": location_of("live"), "state": "running", "node_count": 1, "failed_because": null }),
        json!({ "stream": parked, "display_name": "parked", "location": location_of("parked"), "state": "stopped", "node_count": null, "failed_because": null }),
        json!({ "stream": waiting, "display_name": "waiting", "location": location_of("waiting"), "state": "running", "node_count": null, "failed_because": null }),
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

/// `expose_port` changes a loaded stream's port live, recording nothing for
/// one its host loaded, and records the owner's ruling on a stopped stream once
/// its recorded graph holds the node.
#[tokio::test(flavor = "multi_thread")]
async fn expose_port_changes_a_loaded_port_live_and_records_a_stopped_streams_ruling() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);

    let live_exposed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "expose_port",
            json!({ "stream": live, "node": "source", "port": "video", "level": "public" }),
        )
        .await,
    )
    .expect("the live port is made public");
    assert_eq!(
        live_exposed,
        json!({ "stream": live, "node": "source", "port": "video", "level": "public", "recorded": false })
    );
    let live_graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": live }),
        )
        .await,
    );
    assert_eq!(
        live_graph["exposed"],
        json!([{ "node": "source", "port": "video", "level": "public" }]),
        "`graph` shows the new level at once: {live_graph}"
    );

    let recorded = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "expose_port",
            json!({ "stream": parked, "node": "source", "port": "video", "level": "internal" }),
        )
        .await,
    )
    .expect("the stopped stream's ruling is recorded");
    assert_eq!(recorded["recorded"], true);
    let parked_record = engine
        .current_record_of(&parked)
        .expect("the parked record");
    assert_eq!(parked_record.exposure_rulings.len(), 1);
    assert_eq!(
        parked_record.exposure_rulings[0].level,
        OutputPortExposureLevel::Internal
    );

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "expose_port",
            json!({ "stream": parked, "node": "nowhere", "port": "video", "level": "private" }),
        )
        .await,
    )
    .expect_err("a node the recorded graph lacks is refused");
    assert!(
        refusal.contains("nowhere") && refusal.contains("source"),
        "{refusal}"
    );
}

/// Stop, start and remove through the tools: each answers in the runtime's
/// words, refuses what it cannot do naming why, and leaves the state
/// directory as it says.
#[tokio::test(flavor = "multi_thread")]
async fn stop_start_and_remove_act_on_the_streams_the_runtime_holds() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
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

    assert_eq!(
        call("stop_stream", json!({ "stream": live }))
            .await
            .unwrap(),
        json!({ "stream": live, "stopped": true })
    );
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());

    assert_eq!(
        call("remove_stream", json!({ "stream": parked }))
            .await
            .unwrap(),
        json!({ "stream": parked, "unloaded": false, "forgotten": true })
    );
    assert_eq!(engine.current_record_of(&parked), None);
    let refusal = call("remove_stream", json!({ "stream": parked }))
        .await
        .expect_err("a stream the runtime no longer holds is refused");
    assert!(
        refusal.contains(&parked) && refusal.contains("Loaded: none"),
        "{refusal}"
    );
}

/// `rename_stream` shows a stream under a new display name by its id alone:
/// a recorded stream's record carries it, a loaded one's listing shows it,
/// and the id the call named is the id it answers.
#[tokio::test(flavor = "multi_thread")]
async fn rename_stream_shows_a_stream_under_a_new_display_name_and_records_a_recorded_one() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
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
/// name, with why and how to run its stream again, until `remove_stream`
/// forgets it.
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
            "graph": the_graph_of_a_function_named("main"),
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

    assert_eq!(
        call("remove_stream", json!({ "stream": "main" }))
            .await
            .unwrap(),
        json!({ "stream": "main", "unloaded": false, "forgotten": true })
    );
    assert_eq!(
        call("list_streams", json!({})).await.unwrap(),
        json!({ "streams": [] })
    );
}

/// A recorded stream whose interpreter is gone does not start, refused naming the
/// interpreter and how to bring it back; a run in a project with no venv is
/// refused pointing at `uv sync`.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_or_run_without_the_projects_interpreter_is_refused_pointing_at_uv_sync() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("gone"));
    let gone = engine.a_stream_recorded_and_not_loaded(&project, "gone", true);
    std::fs::remove_file(project.interpreter()).expect("the interpreter goes");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "start_stream",
            json!({ "stream": gone }),
        )
        .await,
    )
    .expect_err("a recorded stream whose interpreter is gone does not start");
    assert!(
        refusal.contains(&project.interpreter().display().to_string())
            && refusal.contains("uv sync"),
        "{refusal}"
    );

    let project_with_no_venv = tempfile::tempdir().expect("a project directory");
    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "run_stream",
            json!({ "project_directory": project_with_no_venv.path() }),
        )
        .await,
    )
    .expect_err("a project with no venv does not run");
    assert!(refusal.contains("uv sync"), "{refusal}");
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
    let recorded_stream_ids: Vec<String> = engine
        .stream_records()
        .read_every()
        .into_iter()
        .map(|read| read.expect("the record reads").stream_id().to_string())
        .collect();
    assert_eq!(recorded_stream_ids, [gone], "a refused run records nothing");
}

// ============================================================================
// End to end on the engine's own run — the start creates its GPU context
// ============================================================================

/// A project's stream function run over a `/mcp/stdio` connection is recorded
/// and runs on once the connection drops, and stop, start and remove act on it
/// through the tools.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_stream_end_to_end_outlives_its_connection_through_stop_start_and_remove() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling_warning(
        the_graph_of_a_function_named("camera"),
        &[CROSS_FLOOR_WARNING_A_COMPILE_WROTE],
    );
    let camera_stream_id = project.stream_id_of("camera");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let run = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the stream runs");
    assert_eq!(
        run,
        json!({
            "stream": camera_stream_id,
            "display_name": "camera",
            "stream_instance": engine
                .engine
                .loaded_stream_of_the_id(&camera_stream_id)
                .unwrap()
                .stream_tag()
                .to_string(),
            "location": {
                "project_directory": project.path(),
                "stream_function": "stream:camera",
            },
            "node_count": 1,
            "replaced_a_recorded_stream": false,
            "compile_warnings": [CROSS_FLOOR_WARNING_A_COMPILE_WROTE],
        })
    );
    assert!(
        engine.current_record_of(&camera_stream_id).is_some(),
        "the run is recorded"
    );

    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        [camera_stream_id.clone()]
    );

    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    assert_eq!(
        call("stop_stream", json!({ "stream": camera_stream_id }))
            .await
            .unwrap(),
        json!({ "stream": camera_stream_id, "stopped": true })
    );
    assert_eq!(
        call("start_stream", json!({ "stream": camera_stream_id }))
            .await
            .unwrap(),
        json!({ "stream": camera_stream_id, "node_count": 1 })
    );
    assert_eq!(
        call("remove_stream", json!({ "stream": camera_stream_id }))
            .await
            .unwrap(),
        json!({ "stream": camera_stream_id, "unloaded": true, "forgotten": true })
    );
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
}

/// `tatolab dev`'s reload: a run of a recorded location replaces its stream
/// under the same id and another instance, whichever connection carries it; a
/// run whose compile fails leaves the running stream loaded; a run of another
/// function in the project loads beside it under its own id; and closing every
/// connection unloads nothing.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_of_a_recorded_location_replaces_its_stream_from_any_connection() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project =
        ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("camera"));
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let first_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let other_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let run = json!({ "project_directory": project.path() });
    let camera_stream_id = project.stream_id_of("camera");
    let stream_tag_loaded_as_camera = || {
        engine
            .engine
            .loaded_stream_of_the_id(&camera_stream_id)
            .unwrap()
            .stream_tag()
    };

    tool_answer(&tool_call_over_the_connection(&first_connection, "run_stream", run.clone()).await)
        .expect("the stream runs");
    let first_load = stream_tag_loaded_as_camera();

    project.fail_to_compile_printing("SyntaxError: invalid syntax");
    let refusal = tool_answer(
        &tool_call_over_the_connection(&first_connection, "run_stream", run.clone()).await,
    )
    .expect_err("a bad save is refused");
    assert!(refusal.contains("SyntaxError"), "{refusal}");
    assert_eq!(
        stream_tag_loaded_as_camera(),
        first_load,
        "a bad save leaves the running stream loaded"
    );

    project.compile_to_warning(the_graph_of_a_function_named("camera"), &[]);
    let replaced =
        tool_answer(&tool_call_over_the_connection(&other_connection, "run_stream", run).await)
            .expect("another connection's run of the location replaces its stream");
    assert_eq!(replaced["stream"], camera_stream_id.as_str());
    assert_eq!(replaced["replaced_a_recorded_stream"], true);
    assert_ne!(stream_tag_loaded_as_camera(), first_load);
    assert_eq!(
        replaced["stream_instance"],
        stream_tag_loaded_as_camera().to_string()
    );
    assert_ne!(replaced["stream_instance"], first_load.to_string());

    project.compile_to_warning(the_graph_of_a_function_named("studio"), &[]);
    let studio = tool_answer(
        &tool_call_over_the_connection(
            &first_connection,
            "run_stream",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of another function loads it");
    let studio_stream_id = project.stream_id_of("studio");
    assert_eq!(studio["stream"], studio_stream_id.as_str());
    let mut both_stream_ids = vec![camera_stream_id.clone(), studio_stream_id];
    both_stream_ids.sort();
    assert_eq!(engine.engine.ids_of_the_loaded_streams(), both_stream_ids);

    drop(other_connection);
    drop(first_connection);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(engine.engine.ids_of_the_loaded_streams(), both_stream_ids);
}
