// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream tools over a real engine: a stream attached over a `/mcp/stdio`
//! connection unloads when the connection drops while a kept one stays,
//! `logs` pages a stream's records by sequence number, and `list_streams`,
//! `stop_stream`, `start_stream`, `remove_stream` and `expose_port` act on an
//! engine keeping its streams in a temporary state directory.
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
use streamlib::sdk::logging::LoadedStreamLogRecordsPage;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::{
    BoxFuture, ExchangedPublishedSurfaceFramePngImage, KeptStreamRecord,
    KeptStreamRecordsInTheStateDirectory, LoadedStreamTag,
    OperationsOnTheStreamsLoadedInThisRuntime, OptionsForLoadingOneStream,
    OutputPortExposureOutcome, RunStreamRequest, Runner, RuntimeOperations, StreamEnvironment,
    StreamListing, StreamRemoveOutcome, StreamRunOutcome, StreamStartOutcome, StreamStopOutcome,
};

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{first_text_block_json, tool_call_result};
use crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream;

/// How long a test waits for what an engine does on another thread — an
/// unload after a connection dropped, a record reaching a stream's log.
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

/// The graph a stream function named `stream_name` compiles to: one
/// [`StreamToolsTestSource`] named `source`, exposing nothing.
fn the_graph_of_a_function_named(stream_name: &str) -> Value {
    json!({
        "stream": stream_name,
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
    /// standard error, then prints `graph`.
    fn compile_to_warning(&self, graph: Value, warnings: &[&str]) {
        let compile_document = json!({
            "stream_graph": graph,
            "project_directory": self.path(),
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

    fn stream_environment(&self) -> StreamEnvironment {
        StreamEnvironment {
            project_directory: self.path().to_path_buf(),
            interpreter: self.interpreter(),
        }
    }
}

/// An engine handed a lend directory, keeping its streams in a temporary
/// state directory.
struct AnEngineKeepingItsStreamsInATemporaryStateDirectory {
    engine: Arc<Runner>,
    kept_streams_directory: PathBuf,
    _state_directory: tempfile::TempDir,
}

impl AnEngineKeepingItsStreamsInATemporaryStateDirectory {
    fn new() -> Self {
        register_the_stream_tools_test_source_once();
        let engine = Runner::new().expect("the engine builds");
        engine
            .set_processor_interpreter_lend_directory(PathBuf::from(
                "/opt/tatolab/lib/tatolab/lend",
            ))
            .expect("the lend directory is handed over");
        let state_directory = tempfile::tempdir().expect("a state directory");
        let kept_streams_directory = state_directory.path().join("streams");
        engine
            .keep_streams_in_the_state_directory(&kept_streams_directory)
            .expect("the engine keeps its streams there");
        Self {
            engine,
            kept_streams_directory,
            _state_directory: state_directory,
        }
    }

    fn operations_on_the_loaded_streams(
        &self,
    ) -> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
        Arc::clone(&self.engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>
    }

    fn kept_stream_records(&self) -> KeptStreamRecordsInTheStateDirectory {
        KeptStreamRecordsInTheStateDirectory::open(&self.kept_streams_directory)
            .expect("the kept streams open")
    }

    /// Load `project`'s graph as the attached stream `stream_name`, not
    /// started.
    fn an_attached_stream_loaded_without_its_start(
        &self,
        project: &ProjectWhosePythonPrintsAGraph,
        stream_name: &str,
    ) {
        self.engine
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(the_graph_of_a_function_named(stream_name))
                    .expect("the graph parses"),
                OptionsForLoadingOneStream::in_stream_environment(project.stream_environment())
                    .named(stream_name),
            )
            .expect("the attached stream loads");
    }

    /// Record `project`'s graph as the kept stream `stream_name`, stopped
    /// when `stopped` says so, and not loaded.
    fn a_kept_stream_recorded_and_not_loaded(
        &self,
        project: &ProjectWhosePythonPrintsAGraph,
        stream_name: &str,
        stopped: bool,
    ) {
        let mut record = KeptStreamRecord::of_a_running_stream(
            stream_name,
            &project.stream_environment(),
            None,
            the_graph_of_a_function_named(stream_name),
        );
        record.stopped = stopped;
        self.kept_stream_records()
            .write(&record)
            .expect("the record is written");
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

/// Poll until `names_of_the_loaded_streams` reads `expected`, and fail
/// naming what it read last.
async fn wait_until_the_loaded_streams_are(engine: &Runner, expected: &[&str]) {
    let deadline =
        tokio::time::Instant::now() + WHAT_THE_ENGINE_DOES_ON_ANOTHER_THREAD_LANDS_WITHIN;
    loop {
        let loaded = engine.names_of_the_loaded_streams();
        if loaded == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the loaded streams never became {expected:?}; they are {loaded:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ============================================================================
// An attached stream ends with its connection
// ============================================================================

/// A cross-floor warning, as a test's compile writes it on its standard error.
const CROSS_FLOOR_WARNING_A_COMPILE_WROTE: &str =
    "tatolab: the cross-floor check found 1 thing binding this app to one floor.";

/// A runtime whose `run_stream` loads an empty stream under the requested
/// name without compiling or starting it — a GPU-free stand-in for the
/// engine's own run, answering [`CROSS_FLOOR_WARNING_A_COMPILE_WROTE`] as its compile's —
/// and hands every other call to the engine.
struct AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt {
    engine: Arc<Runner>,
    project_directory: tempfile::TempDir,
    each_run_by_its_stream_name: std::sync::Mutex<Vec<RunHandedTheCallersStreamTags>>,
}

/// One `run_stream` the test runtime answered: the stream it loaded, the
/// tags of the caller's attached streams it was handed, and the load's tag.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunHandedTheCallersStreamTags {
    stream_name: String,
    stream_tags_attached_to_the_caller: Vec<LoadedStreamTag>,
    stream_tag_of_the_load: LoadedStreamTag,
}

impl AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt {
    fn new() -> Self {
        Self {
            engine: Runner::new().expect("the engine builds"),
            project_directory: tempfile::tempdir().expect("a project directory"),
            each_run_by_its_stream_name: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl OperationsOnTheStreamsLoadedInThisRuntime
    for AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt
{
    fn this_runtimes_name(&self) -> &str {
        self.engine.this_runtimes_name()
    }
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.engine
            .runtime_operations_of_the_stream_a_call_names(stream_name)
    }
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.engine
            .node_catalog_of_the_stream_a_call_names(stream_name)
    }
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.engine
            .node_types_described_in_the_interpreter_of_the_stream_a_call_names(stream_name)
    }
    fn names_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine.names_of_the_loaded_streams()
    }
    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LoadedStreamLogRecordsPage> {
        self.engine
            .log_records_of_the_stream_a_call_names(stream_name, after, max_count)
    }
    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        let stream = self.engine.load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(self.project_directory.path()).named(
                request
                    .stream_name
                    .expect("the test names every stream it runs"),
            ),
        )?;
        self.each_run_by_its_stream_name
            .lock()
            .expect("no test thread panicked holding the runs")
            .push(RunHandedTheCallersStreamTags {
                stream_name: stream.stream_name().to_string(),
                stream_tags_attached_to_the_caller: request.stream_tags_attached_to_the_caller,
                stream_tag_of_the_load: stream.stream_tag(),
            });
        Ok(StreamRunOutcome {
            stream_name: stream.stream_name().to_string(),
            stream_tag: stream.stream_tag(),
            project_directory: request.project_directory,
            node_count: 0,
            replaced_the_kept_record: false,
            compile_warnings: vec![CROSS_FLOOR_WARNING_A_COMPILE_WROTE.to_string()],
        })
    }
    fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::stop_stream(self.engine.as_ref(), stream_name)
    }
    fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::start_stream(self.engine.as_ref(), stream_name)
    }
    fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::remove_stream(self.engine.as_ref(), stream_name)
    }
    fn list_streams(&self) -> Vec<StreamListing> {
        OperationsOnTheStreamsLoadedInThisRuntime::list_streams(self.engine.as_ref())
    }
    fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        OperationsOnTheStreamsLoadedInThisRuntime::expose_port(
            self.engine.as_ref(),
            stream_name,
            node,
            port,
            level,
        )
    }
    fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool {
        OperationsOnTheStreamsLoadedInThisRuntime::unload_the_attached_stream_if_still_the_same(
            self.engine.as_ref(),
            stream_name,
            stream_tag,
        )
    }
    fn exchange_published_surface_id_for_png_image_bytes_async(
        &self,
        published_surface_id: String,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> BoxFuture<'_, Result<ExchangedPublishedSurfaceFramePngImage>> {
        self.engine
            .exchange_published_surface_id_for_png_image_bytes_async(
                published_surface_id,
                downscale_long_edge_pixel_cap,
            )
    }
}

fn a_run_request_naming(stream_name: &str, keep: bool) -> Value {
    json!({
        "project_directory": "/home/someone/projects/camera",
        "name": stream_name,
        "keep": keep,
    })
}

/// The connection is the attached stream's lifetime: dropping it — a killed
/// CLI, a closed terminal — unloads the stream it attached, while a stream it
/// ran kept stays loaded.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_stream_unloads_when_its_connection_drops_and_a_kept_one_stays() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let attached = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            a_run_request_naming("attached-one", false),
        )
        .await,
    )
    .expect("the attached run is answered");
    assert_eq!(attached["stream"], "attached-one");
    assert_eq!(attached["kept"], false);
    assert_eq!(
        attached["compile_warnings"],
        json!([CROSS_FLOOR_WARNING_A_COMPILE_WROTE]),
        "the run's result carries what its compile wrote to its standard error"
    );
    tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            a_run_request_naming("kept-one", true),
        )
        .await,
    )
    .expect("the kept run is answered");
    wait_until_the_loaded_streams_are(&engine, &["attached-one", "kept-one"]).await;

    drop(client);

    wait_until_the_loaded_streams_are(&engine, &["kept-one"]).await;
}

/// A closed connection unloads only the load it attached: a stream since
/// stopped and run again under the same name — by another connection — stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_connection_leaves_a_stream_that_took_the_attached_name_since() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let operations_on_the_loaded_streams =
        Arc::clone(&runtime) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let served = LocalApiServedOnAFreshSocket::over(Arc::clone(&operations_on_the_loaded_streams));
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            a_run_request_naming("camera", false),
        )
        .await,
    )
    .expect("the attached run is answered");
    let stopped = tool_answer(
        &tool_call_over_the_connection(&client, "stop_stream", json!({ "stream": "camera" })).await,
    )
    .expect("the attached stream stops");
    assert_eq!(
        stopped,
        json!({ "stream": "camera", "stopped": true, "kept": false })
    );
    tool_answer(
        &tool_call_result(
            operations_on_the_loaded_streams,
            "run_stream",
            a_run_request_naming("camera", true),
        )
        .await,
    )
    .expect("the one-shot kept run is answered");

    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(engine.names_of_the_loaded_streams(), ["camera"]);
}

/// An attached run is handed the tags of the streams its own connection
/// attached, and a kept run or another connection's run none, so the engine
/// replaces a stream only for the connection that attached it.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_run_is_handed_the_stream_tags_its_own_connection_attached_and_no_others() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let served = LocalApiServedOnAFreshSocket::over(
        Arc::clone(&runtime) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>
    );
    let first_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let second_connection = rmcp_client_over_the_upgraded_stream(&served).await;

    for (connection, stream_name, keep) in [
        (&first_connection, "camera", false),
        (&second_connection, "preview", false),
        (&first_connection, "microphone", false),
        (&first_connection, "kept-one", true),
    ] {
        tool_answer(
            &tool_call_over_the_connection(
                connection,
                "run_stream",
                a_run_request_naming(stream_name, keep),
            )
            .await,
        )
        .unwrap_or_else(|refusal| panic!("`{stream_name}` runs: {refusal}"));
    }

    let each_run = runtime.each_run_by_its_stream_name.lock().unwrap().clone();
    let stream_tag_of = |stream_name: &str| {
        each_run
            .iter()
            .find(|run| run.stream_name == stream_name)
            .expect("the stream ran")
            .stream_tag_of_the_load
    };
    let stream_tags_each_run_was_handed: Vec<(&str, Vec<LoadedStreamTag>)> = each_run
        .iter()
        .map(|run| {
            (
                run.stream_name.as_str(),
                run.stream_tags_attached_to_the_caller.clone(),
            )
        })
        .collect();
    assert_eq!(
        stream_tags_each_run_was_handed,
        [
            ("camera", vec![]),
            ("preview", vec![]),
            ("microphone", vec![stream_tag_of("camera")]),
            ("kept-one", vec![]),
        ]
    );
}

/// Stopping the local API ends every upgraded connection, and each unloads
/// what it attached rather than leaving it to outlive the API.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_stream_unloads_when_the_local_api_stops_serving() {
    let runtime = Arc::new(AnEngineWhoseRunLoadsAnEmptyStreamWithoutStartingIt::new());
    let engine = Arc::clone(&runtime.engine);
    let mut served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            a_run_request_naming("attached-one", false),
        )
        .await,
    )
    .expect("the attached run is answered");

    served.stop_serving();

    wait_until_the_loaded_streams_are(&engine, &[]).await;
    drop(client);
}

// ============================================================================
// logs
// ============================================================================

/// `logs` reads a stream's records by sequence number: `after` skips what was
/// read, `count` bounds the page, and `next_after` is where to read on from.
#[tokio::test(flavor = "multi_thread")]
async fn logs_pages_a_streams_records_by_sequence_number() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let engine = Runner::new().expect("the engine builds");
    let stream = engine
        .load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                .named("paged"),
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
// list, stop, start, remove and expose against an engine keeping its streams
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn list_streams_names_each_stream_attached_kept_or_stopped() {
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
    engine.an_attached_stream_loaded_without_its_start(&project, "live");
    engine.a_kept_stream_recorded_and_not_loaded(&project, "parked", true);
    engine.a_kept_stream_recorded_and_not_loaded(&project, "waiting", false);

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");

    let project_directory = project.path().display().to_string();
    assert_eq!(
        listed,
        json!({
            "streams": [
                { "name": "live", "state": "attached", "project_directory": project_directory, "node_count": 1 },
                { "name": "parked", "state": "stopped", "project_directory": project_directory, "node_count": null },
                { "name": "waiting", "state": "kept", "project_directory": project_directory, "node_count": null },
            ]
        })
    );
}

/// `expose_port` changes a loaded stream's port live, recording nothing for an
/// attached one, and records the owner's ruling on a stopped kept stream once
/// its recorded graph holds the node.
#[tokio::test(flavor = "multi_thread")]
async fn expose_port_changes_a_loaded_port_live_and_records_a_kept_streams_ruling() {
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
    engine.an_attached_stream_loaded_without_its_start(&project, "live");
    engine.a_kept_stream_recorded_and_not_loaded(&project, "parked", true);

    let live = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "expose_port",
            json!({ "stream": "live", "node": "source", "port": "video", "level": "public" }),
        )
        .await,
    )
    .expect("the live port is made public");
    assert_eq!(
        live,
        json!({ "stream": "live", "node": "source", "port": "video", "level": "public", "recorded": false })
    );
    let live_graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": "live" }),
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
            json!({ "stream": "parked", "node": "source", "port": "video", "level": "internal" }),
        )
        .await,
    )
    .expect("the stopped stream's ruling is recorded");
    assert_eq!(recorded["recorded"], true);
    let parked_record = engine
        .kept_stream_records()
        .read("parked")
        .unwrap()
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
            json!({ "stream": "parked", "node": "nowhere", "port": "video", "level": "private" }),
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
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("live"));
    engine.an_attached_stream_loaded_without_its_start(&project, "live");
    engine.a_kept_stream_recorded_and_not_loaded(&project, "parked", true);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let refusal = call("start_stream", json!({ "stream": "live" }))
        .await
        .expect_err("a loaded stream does not start");
    assert!(
        refusal.contains("already loaded") && refusal.contains("attached"),
        "{refusal}"
    );
    let refusal = call("start_stream", json!({ "stream": "nothing" }))
        .await
        .expect_err("a name no record holds does not start");
    assert!(
        refusal.contains("nothing") && refusal.contains("parked"),
        "{refusal}"
    );
    let refusal = call("stop_stream", json!({ "stream": "parked" }))
        .await
        .expect_err("a stopped stream does not stop again");
    assert!(refusal.contains("already stopped"), "{refusal}");

    assert_eq!(
        call("stop_stream", json!({ "stream": "live" }))
            .await
            .unwrap(),
        json!({ "stream": "live", "stopped": true, "kept": false })
    );
    assert!(engine.engine.names_of_the_loaded_streams().is_empty());

    assert_eq!(
        call("remove_stream", json!({ "stream": "parked" }))
            .await
            .unwrap(),
        json!({ "stream": "parked", "unloaded": false, "forgotten": true })
    );
    assert_eq!(engine.kept_stream_records().read("parked").unwrap(), None);
    let refusal = call("remove_stream", json!({ "stream": "parked" }))
        .await
        .expect_err("a stream the runtime no longer holds is refused");
    assert!(
        refusal.contains("parked") && refusal.contains("Loaded: none"),
        "{refusal}"
    );
}

/// A kept stream whose interpreter is gone does not start, refused naming the
/// interpreter and how to bring it back; a run in a project with no venv is
/// refused pointing at `uv sync`.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_or_run_without_the_projects_interpreter_is_refused_pointing_at_uv_sync() {
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("gone"));
    engine.a_kept_stream_recorded_and_not_loaded(&project, "gone", true);
    std::fs::remove_file(project.interpreter()).expect("the interpreter goes");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "start_stream",
            json!({ "stream": "gone" }),
        )
        .await,
    )
    .expect_err("a kept stream whose interpreter is gone does not start");
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
            json!({ "project_directory": project_with_no_venv.path(), "keep": true }),
        )
        .await,
    )
    .expect_err("a project with no venv does not run");
    assert!(refusal.contains("uv sync"), "{refusal}");
    assert!(engine.engine.names_of_the_loaded_streams().is_empty());
    let recorded_stream_names: Vec<String> = engine
        .kept_stream_records()
        .read_every()
        .into_iter()
        .map(|read| read.expect("the record reads").stream_name)
        .collect();
    assert_eq!(
        recorded_stream_names,
        ["gone"],
        "a refused run records nothing"
    );
}

// ============================================================================
// End to end on the engine's own run — the start creates its GPU context
// ============================================================================

/// A project's stream function run attached over a `/mcp/stdio` connection,
/// and another kept over a one-shot call: dropping the connection unloads
/// the attached stream, the kept one runs on, and stop, start and remove act
/// on it through the tools.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_stream_end_to_end_attached_and_kept_through_stop_start_and_remove() {
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let attached_project = ProjectWhosePythonPrintsAGraph::compiling_warning(
        the_graph_of_a_function_named("attached"),
        &[CROSS_FLOOR_WARNING_A_COMPILE_WROTE],
    );
    let kept_project =
        ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("kept"));
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let attached = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "run_stream",
            json!({ "project_directory": attached_project.path(), "keep": false }),
        )
        .await,
    )
    .expect("the attached stream runs");
    assert_eq!(
        attached,
        json!({
            "stream": "attached",
            "kept": false,
            "project_directory": attached_project.path(),
            "node_count": 1,
            "replaced_the_kept_record": false,
            "compile_warnings": [CROSS_FLOOR_WARNING_A_COMPILE_WROTE],
        })
    );
    let kept = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "run_stream",
            json!({ "project_directory": kept_project.path(), "keep": true }),
        )
        .await,
    )
    .expect("the kept stream runs");
    assert_eq!(kept["kept"], true);
    assert_eq!(
        kept["compile_warnings"],
        json!([]),
        "a compile that wrote nothing to its standard error answers no warning"
    );
    wait_until_the_loaded_streams_are(&engine.engine, &["attached", "kept"]).await;

    drop(client);
    wait_until_the_loaded_streams_are(&engine.engine, &["kept"]).await;

    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    assert_eq!(
        call("stop_stream", json!({ "stream": "kept" }))
            .await
            .unwrap(),
        json!({ "stream": "kept", "stopped": true, "kept": true })
    );
    assert_eq!(
        call("start_stream", json!({ "stream": "kept" }))
            .await
            .unwrap(),
        json!({ "stream": "kept", "node_count": 1 })
    );
    assert_eq!(
        call("remove_stream", json!({ "stream": "kept" }))
            .await
            .unwrap(),
        json!({ "stream": "kept", "unloaded": true, "forgotten": true })
    );
    assert!(engine.engine.names_of_the_loaded_streams().is_empty());
}

/// `tatolab dev`'s reload: the connection that attached a stream runs it
/// again and the engine replaces it, a run whose compile fails leaves the
/// running stream loaded and attached, another connection's run of the name
/// is refused, and closing the connection unloads the stream that replaced
/// the first.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn an_attached_run_again_on_its_own_connection_replaces_its_stream_and_no_other_connections()
{
    let engine = AnEngineKeepingItsStreamsInATemporaryStateDirectory::new();
    let project =
        ProjectWhosePythonPrintsAGraph::compiling(the_graph_of_a_function_named("camera"));
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let attaching_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let other_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let attached_run = json!({ "project_directory": project.path(), "keep": false });
    let stream_tag_loaded_as_camera = || {
        engine
            .engine
            .loaded_stream_named("camera")
            .unwrap()
            .stream_tag()
    };

    tool_answer(
        &tool_call_over_the_connection(&attaching_connection, "run_stream", attached_run.clone())
            .await,
    )
    .expect("the attached stream runs");
    let first_load = stream_tag_loaded_as_camera();

    project.fail_to_compile_printing("SyntaxError: invalid syntax");
    let refusal = tool_answer(
        &tool_call_over_the_connection(&attaching_connection, "run_stream", attached_run.clone())
            .await,
    )
    .expect_err("a bad save is refused");
    assert!(refusal.contains("SyntaxError"), "{refusal}");
    assert_eq!(
        stream_tag_loaded_as_camera(),
        first_load,
        "a bad save leaves the running stream loaded"
    );

    project.compile_to_warning(the_graph_of_a_function_named("camera"), &[]);
    let refusal = tool_answer(
        &tool_call_over_the_connection(&other_connection, "run_stream", attached_run.clone()).await,
    )
    .expect_err("another connection's run of the name is refused");
    assert!(refusal.contains("already loaded"), "{refusal}");
    assert_eq!(stream_tag_loaded_as_camera(), first_load);

    let replaced = tool_answer(
        &tool_call_over_the_connection(&attaching_connection, "run_stream", attached_run).await,
    )
    .expect("the attaching connection's run replaces its stream");
    assert_eq!(replaced["stream"], "camera");
    assert_ne!(stream_tag_loaded_as_camera(), first_load);

    drop(other_connection);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(engine.engine.names_of_the_loaded_streams(), ["camera"]);
    drop(attaching_connection);
    wait_until_the_loaded_streams_are(&engine.engine, &[]).await;
}
