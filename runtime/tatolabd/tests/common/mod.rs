// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Running the built `tatolabd` from a runtime unit of its own, with the
//! machine's directories under a short test root of its own.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};
use streamlib::sdk::processor_interpreter::{
    BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, lend_directory_in_the_runtime_unit,
    processor_interpreter_bootstrap_path,
};
use streamlib::sdk::runtime::{
    STREAM_RECORD_SCHEMA_VERSION, StreamLocation, StreamRecord, StreamRecordAsRead,
    StreamRecordsInTheStateDirectory,
};

/// The line `tatolabd` logs once its local API is served and its re-loads
/// queued.
pub const THE_RUNTIME_IS_SERVING_LOG_LINE: &str = "the runtime is serving:";

/// How long a `tatolabd` holding no started stream takes to start serving.
pub const A_RUNTIME_STARTS_SERVING_WITHIN: Duration = Duration::from_secs(60);

/// How long a `tatolabd` takes to exit once interrupted.
pub const AN_INTERRUPTED_RUNTIME_EXITS_WITHIN: Duration = Duration::from_secs(30);

/// A temporary runtime unit: `bin/tatolabd`, and the lend beside it unless the
/// test asked for none.
pub struct TemporaryRuntimeUnit {
    runtime_unit_root: tempfile::TempDir,
    pub tatolabd: PathBuf,
}

impl TemporaryRuntimeUnit {
    /// `bin/tatolabd` beside the lend, holding the bootstrap.
    pub fn with_its_lend() -> Self {
        let runtime_unit = Self::without_a_lend();
        let bootstrap = processor_interpreter_bootstrap_path(&lend_directory_in_the_runtime_unit(
            runtime_unit.runtime_unit_root(),
        ));
        std::fs::create_dir_all(bootstrap.parent().expect("the bootstrap has a directory"))
            .expect("the lend is created");
        std::fs::write(&bootstrap, "").expect("the bootstrap is written");
        runtime_unit
    }

    /// The directory holding `bin/` and the lend.
    pub fn runtime_unit_root(&self) -> &Path {
        self.runtime_unit_root.path()
    }

    /// `bin/tatolabd` and nothing beside it.
    ///
    /// Linked, or else copied, never symlinked: `tatolabd` finds its lend from
    /// its own canonical path, which a symlink would point back at the build.
    pub fn without_a_lend() -> Self {
        let runtime_unit_root = tempfile::Builder::new()
            .prefix("tatolabd-runtime-unit-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("a temporary runtime unit");
        let bin_directory = runtime_unit_root
            .path()
            .join(BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT);
        std::fs::create_dir_all(&bin_directory).expect("bin/ is created");
        let tatolabd = bin_directory.join("tatolabd");
        let built_tatolabd = Path::new(env!("CARGO_BIN_EXE_tatolabd"));
        if std::fs::hard_link(built_tatolabd, &tatolabd).is_err() {
            std::fs::copy(built_tatolabd, &tatolabd).expect("tatolabd is copied into bin/");
        }
        Self {
            runtime_unit_root,
            tatolabd,
        }
    }
}

/// A port both UDP and TCP bind on `host` now.
pub fn a_port_free_for_udp_and_tcp_on(host: &str) -> u16 {
    for _ in 0..50 {
        let probe = std::net::TcpListener::bind(format!("{host}:0")).expect("an ephemeral port");
        let port = probe.local_addr().expect("a bound port").port();
        drop(probe);
        if the_port_is_free_for_udp_and_tcp_on(host, port) {
            return port;
        }
    }
    panic!("no port free for both UDP and TCP on {host}");
}

/// Whether both UDP and TCP bind `port` on `host`.
pub fn the_port_is_free_for_udp_and_tcp_on(host: &str, port: u16) -> bool {
    std::net::TcpListener::bind(format!("{host}:{port}")).is_ok()
        && std::net::UdpSocket::bind(format!("{host}:{port}")).is_ok()
}

/// The body of `GET <path>` on loopback `port`, or `None` when nothing answers
/// it with a 200.
pub fn http_get_body_on_loopback(port: u16, path: &str) -> Option<String> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    let (head, body) = answer.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200")
        .then(|| body.trim().to_string())
}

/// The machine a test's `tatolabd` runs on: `TATOLAB_TEST_MACHINE_ROOT`, under
/// which its runtime directory and state directory sit, and on Linux its lock,
/// plus its `STREAMLIB_HOME` and a project directory. On macOS its lock is the
/// loopback HTTP port it names, one of its own.
///
/// Short and under `/tmp`, so the local API socket's path fits `sun_path`.
pub struct TatolabTestMachineRoot {
    machine_root: tempfile::TempDir,
    loopback_http_port_on_macos: Option<u16>,
}

impl TatolabTestMachineRoot {
    pub fn new() -> Self {
        let machine_root = tempfile::Builder::new()
            .prefix("tl-")
            .tempdir_in("/tmp")
            .expect("a temporary machine root");
        for directory in ["home", "p"] {
            std::fs::create_dir_all(machine_root.path().join(directory))
                .expect("a directory under the machine root is created");
        }
        Self {
            machine_root,
            loopback_http_port_on_macos: cfg!(target_os = "macos")
                .then(|| a_port_free_for_udp_and_tcp_on("127.0.0.1")),
        }
    }

    pub fn path(&self) -> &Path {
        self.machine_root.path()
    }

    /// The runtime directory a test build resolves: `<root>/run/`.
    pub fn runtime_directory(&self) -> PathBuf {
        self.path().join("run")
    }

    /// The local API's fixed socket in the runtime directory.
    pub fn local_api_socket_path(&self) -> PathBuf {
        self.runtime_directory().join("local-api.sock")
    }

    /// The state directory a test build resolves: `<root>/state/`.
    pub fn state_directory(&self) -> PathBuf {
        self.path().join("state")
    }

    /// `<state>/streams/`, one `<stream id>.json` record per recorded stream.
    pub fn streams_directory(&self) -> PathBuf {
        self.state_directory().join("streams")
    }

    /// `<state>/runtime-run-in-progress`, which a crash leaves behind.
    pub fn runtime_run_in_progress_record_path(&self) -> PathBuf {
        self.state_directory().join("runtime-run-in-progress")
    }

    /// `<state>/logs/`, the runtime's own log.
    pub fn runtime_log_directory(&self) -> PathBuf {
        self.state_directory().join("logs")
    }

    /// A directory standing in for a stream's project.
    pub fn project_directory(&self) -> PathBuf {
        self.path().join("p")
    }

    /// The stream records under this machine's state directory, as the
    /// engine writes and reads them.
    pub fn stream_records(&self) -> StreamRecordsInTheStateDirectory {
        StreamRecordsInTheStateDirectory::open(&self.streams_directory())
            .expect("the streams directory opens")
    }

    /// The record of `stream_id` the runtime wrote under this schema, `None`
    /// when there is none.
    pub fn current_stream_record_of(&self, stream_id: &str) -> Option<StreamRecord> {
        match self
            .stream_records()
            .read(stream_id)
            .expect("the record reads")
        {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(earlier) => panic!("`{stream_id}` is an earlier runtime's record: {earlier:?}"),
            None => None,
        }
    }

    /// A `tatolabd` command on this machine that inherits none of the
    /// engine's settings from the test's own environment.
    pub fn tatolabd_command(&self, tatolabd: &Path) -> Command {
        let mut tatolabd_command = Command::new(tatolabd);
        tatolabd_command
            .current_dir(self.path())
            .env("TATOLAB_TEST_MACHINE_ROOT", self.path())
            .env("STREAMLIB_HOME", self.path().join("home"))
            .env_remove("STREAMLIB_RUNTIME_ID")
            .env_remove("STREAMLIB_QUIET")
            .env_remove("STREAMLIB_DANGEROUSLY_DEFER_LOGGING_TO_HOST")
            .stdin(Stdio::null());
        if let Some(loopback_http_port) = self.loopback_http_port_on_macos {
            use streamlib_runtime_client_contract::machine_directories_test_root::TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE;
            tatolabd_command.env(
                TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
                loopback_http_port.to_string(),
            );
        }
        tatolabd_command
    }

    /// A `tatolabd` command whose Vulkan loader finds no driver, so a stream
    /// it loads is refused at its start, wherever the test runs.
    pub fn tatolabd_command_with_no_vulkan_driver(&self, tatolabd: &Path) -> Command {
        let no_vulkan_driver_file = self.path().join("no-vulkan-driver-here.json");
        let mut tatolabd_command = self.tatolabd_command(tatolabd);
        tatolabd_command
            .env("VK_DRIVER_FILES", &no_vulkan_driver_file)
            .env("VK_ICD_FILENAMES", &no_vulkan_driver_file);
        tatolabd_command
    }
}

/// A stream of one native numbered message writer.
pub fn a_native_only_stream_graph(stream_id: &str) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_id,
        "nodes": [{
            "name": "numbers",
            "type": "tatolabd::numbered_message_writer_test_node::NumberedMessageWriterTestNode",
        }],
    })
}

/// The class import path the crash-on-demand test node registers under.
pub const CRASH_ON_DEMAND_TEST_NODE_TYPE: &str =
    "tatolabd::crash_on_demand_test_node::CrashOnDemandTestNode";

/// A stream of one crash-on-demand node, which crashes the runtime on
/// `SIGSEGV` at the stream's load while `trigger` exists.
pub fn a_stream_graph_crashing_while(stream_id: &str, trigger: &Path) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_id,
        "nodes": [{
            "name": "crasher",
            "type": CRASH_ON_DEMAND_TEST_NODE_TYPE,
            "config": {"crash_while_this_file_exists": trigger, "crash_with": "SIGSEGV"},
        }],
    })
}

/// Start `tatolabd_command` and expect it to crash on `SIGSEGV` at its start.
pub fn expect_the_runtime_to_crash_on_segv(tatolabd_command: Command) {
    let crashed = run_to_exit_within(tatolabd_command, A_RUNTIME_STARTS_SERVING_WITHIN);
    assert_eq!(
        crashed.status.signal(),
        Some(libc::SIGSEGV),
        "the runtime did not crash on SIGSEGV: {}\n{}",
        crashed.status,
        String::from_utf8_lossy(&crashed.stderr)
    );
}

/// The name every test project's `tatolab-project.json` gives it.
pub const TEST_PROJECT_NAME: &str = "rig";

/// The project UUID of the project at `project_directory`, writing its
/// `tatolab-project.json` with a fresh one when it holds none.
pub fn the_project_at(project_directory: &Path) -> uuid::Uuid {
    let project_file_path = project_directory.join("tatolab-project.json");
    if let Ok(project_file_text) = std::fs::read_to_string(&project_file_path) {
        let project_file: serde_json::Value =
            serde_json::from_str(&project_file_text).expect("the project file reads");
        return project_file["project"]
            .as_str()
            .and_then(|project| project.parse().ok())
            .expect("the project file holds its project UUID");
    }
    let project = uuid::Uuid::new_v4();
    std::fs::create_dir_all(project_directory).expect("the project directory");
    std::fs::write(
        &project_file_path,
        serde_json::json!({ "project": project, "name": TEST_PROJECT_NAME, "dependencies": [] })
            .to_string(),
    )
    .expect("the project's tatolab-project.json");
    project
}

/// The location of the stream `stream_name` of the project at `project_directory`: its
/// project UUID, and the stream asset UUID drawn from it and the stream's name.
pub fn the_location_of_the_stream(project_directory: &Path, stream_name: &str) -> StreamLocation {
    let project = the_project_at(project_directory);
    StreamLocation {
        project_directory: project_directory.to_path_buf(),
        project,
        stream_asset: uuid::Uuid::new_v5(&project, stream_name.as_bytes()),
    }
}

/// The stream id a first run records the stream `stream_name` of the project at
/// `project_directory` under.
pub fn the_stream_id_of_the_stream(project_directory: &Path, stream_name: &str) -> String {
    the_location_of_the_stream(project_directory, stream_name)
        .stream_id_named(TEST_PROJECT_NAME, stream_name)
}

/// The record of a recorded stream: the stream `stream_name` of the project at
/// `project_directory`, whose graph document holding `graph` it writes, under
/// `interpreter`, keyed by the stream id its location computes to.
pub fn a_stream_record(
    stream_name: &str,
    graph: serde_json::Value,
    project_directory: &Path,
    interpreter: &Path,
    stopped: bool,
) -> StreamRecord {
    let location = the_location_of_the_stream(project_directory, stream_name);
    let mut graph_document = graph;
    graph_document["asset"] = serde_json::json!(location.stream_asset);
    let streams_directory = project_directory.join("streams");
    std::fs::create_dir_all(&streams_directory).expect("the streams directory");
    std::fs::write(
        streams_directory.join(format!("{stream_name}.graph.json")),
        graph_document.to_string(),
    )
    .expect("the graph document");
    StreamRecord {
        schema_version: STREAM_RECORD_SCHEMA_VERSION,
        stream_id: location.stream_id_named(TEST_PROJECT_NAME, stream_name),
        location,
        display_name: stream_name.to_owned(),
        interpreter: interpreter.to_path_buf(),
        stopped,
        failed_because: None,
        runtime_crashes_in_a_row_implicating_it: 0,
        pending_stream_actions: Vec::new(),
    }
}

/// An interpreter path that exists and is executable, for a stream whose
/// native-only graph never starts a processor interpreter.
pub fn an_executable_standing_in_for_the_interpreter() -> PathBuf {
    PathBuf::from("/bin/sh")
}

/// Run `tatolabd_command` to its exit within `budget`, collecting both streams.
pub fn run_to_exit_within(mut tatolabd_command: Command, budget: Duration) -> Output {
    let child = tatolabd_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tatolabd starts");
    let mut spawned = SpawnedTatolabd::collecting_its_streams(child);
    let status = spawned.wait_for_exit_within(budget);
    Output {
        status,
        stdout: spawned.standard_output().into_bytes(),
        stderr: spawned.standard_error().into_bytes(),
    }
}

/// The refusal on standard error, after asserting it exited 1 with nothing on
/// standard output.
pub fn the_refusal_of(output: &Output) -> String {
    let standard_error = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a refusal exits 1, got {}:\n{standard_error}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "tatolabd's standard output carries nothing, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    standard_error
}

/// A running `tatolabd` whose standard streams are read as it writes them.
pub struct SpawnedTatolabd {
    child: Child,
    standard_error_lines: Arc<Mutex<Vec<String>>>,
    standard_output: Arc<Mutex<String>>,
    stream_readers: Vec<std::thread::JoinHandle<()>>,
}

impl SpawnedTatolabd {
    pub fn spawn(mut tatolabd_command: Command) -> Self {
        let child = tatolabd_command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tatolabd starts");
        Self::collecting_its_streams(child)
    }

    /// Spawn `tatolabd_command` and wait until it logs that it is serving.
    pub fn spawn_and_wait_until_serving(tatolabd_command: Command) -> Self {
        let mut spawned = Self::spawn(tatolabd_command);
        spawned.wait_until_standard_error_carries(
            THE_RUNTIME_IS_SERVING_LOG_LINE,
            A_RUNTIME_STARTS_SERVING_WITHIN,
        );
        spawned
    }

    fn collecting_its_streams(mut child: Child) -> Self {
        let standard_error_lines = Arc::new(Mutex::new(Vec::new()));
        let standard_output = Arc::new(Mutex::new(String::new()));
        let child_standard_error = child.stderr.take().expect("stderr is piped");
        let child_standard_output = child.stdout.take().expect("stdout is piped");
        let standard_error_reader = {
            let standard_error_lines = Arc::clone(&standard_error_lines);
            std::thread::spawn(move || {
                for line in BufReader::new(child_standard_error).lines() {
                    let Ok(line) = line else { break };
                    standard_error_lines.lock().unwrap().push(line);
                }
            })
        };
        let standard_output_reader = {
            let standard_output = Arc::clone(&standard_output);
            std::thread::spawn(move || {
                let mut child_standard_output = child_standard_output;
                let mut everything_written = String::new();
                let _ = child_standard_output.read_to_string(&mut everything_written);
                standard_output
                    .lock()
                    .unwrap()
                    .push_str(&everything_written);
            })
        };
        Self {
            child,
            standard_error_lines,
            standard_output,
            stream_readers: vec![standard_error_reader, standard_output_reader],
        }
    }

    pub fn process_id(&self) -> i32 {
        self.child.id() as i32
    }

    /// Block until a line of standard error contains `marker`, or panic naming
    /// what it carried after `budget`.
    pub fn wait_until_standard_error_carries(&mut self, marker: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            if self
                .standard_error_lines
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains(marker))
            {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                self.join_the_stream_readers();
                panic!(
                    "tatolabd exited ({status}) before its standard error carried `{marker}`:\n{}",
                    self.standard_error()
                );
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.join_the_stream_readers();
                panic!(
                    "tatolabd's standard error did not carry `{marker}` within {budget:?}:\n{}",
                    self.standard_error()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Deliver `signal` to the process.
    pub fn deliver(&self, signal: i32) {
        // SAFETY: `kill` reads only its two integer arguments.
        let delivered = unsafe { libc::kill(self.process_id(), signal) };
        assert_eq!(delivered, 0, "the signal was not delivered");
    }

    /// Interrupt the process and assert it exits 0 with nothing on standard
    /// output.
    pub fn interrupt_and_expect_a_clean_exit(&mut self) {
        self.deliver(libc::SIGINT);
        let status = self.wait_for_exit_within(AN_INTERRUPTED_RUNTIME_EXITS_WITHIN);
        assert_eq!(
            status.code(),
            Some(0),
            "an interrupt exits 0, got {status}:\n{}",
            self.standard_error()
        );
        assert_eq!(
            self.standard_output(),
            "",
            "standard output carries nothing"
        );
    }

    /// Wait for the process to exit within `budget`, killing it and panicking
    /// if it does not; the stream readers are joined before this returns.
    pub fn wait_for_exit_within(&mut self, budget: Duration) -> ExitStatus {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.child.try_wait().expect("the child can be waited on") {
                self.join_the_stream_readers();
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.join_the_stream_readers();
                panic!(
                    "tatolabd did not exit within {budget:?}:\n{}",
                    self.standard_error()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn join_the_stream_readers(&mut self) {
        for stream_reader in self.stream_readers.drain(..) {
            let _ = stream_reader.join();
        }
    }

    pub fn standard_error(&self) -> String {
        self.standard_error_lines.lock().unwrap().join("\n")
    }

    pub fn standard_output(&self) -> String {
        self.standard_output.lock().unwrap().clone()
    }
}

impl Drop for SpawnedTatolabd {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// `GET <path>` over the local API socket, retried until it answers 200.
pub fn the_local_apis_answer_to_get(local_api_socket_path: &Path, path: &str) -> serde_json::Value {
    let deadline = Instant::now() + A_RUNTIME_STARTS_SERVING_WITHIN;
    loop {
        if let Some(answer) = get_once(local_api_socket_path, path) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "the local API at {} did not answer `GET {path}`",
            local_api_socket_path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn get_once(local_api_socket_path: &Path, path: &str) -> Option<serde_json::Value> {
    let mut local_api = UnixStream::connect(local_api_socket_path).ok()?;
    local_api
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut response = String::new();
    local_api.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    serde_json::from_str(body).ok()
}

/// What one MCP `tools/call` over the local API answered.
#[derive(Debug)]
pub struct LocalApiToolCallAnswer {
    /// Whether the tool reported a failure.
    pub is_error: bool,
    /// The tool result's first text block.
    pub text: String,
}

impl LocalApiToolCallAnswer {
    /// The first text block as JSON, after asserting the tool succeeded.
    pub fn json(&self) -> serde_json::Value {
        assert!(!self.is_error, "the tool failed: {}", self.text);
        serde_json::from_str(&self.text).expect("the tool's text block is JSON")
    }
}

/// Every file under `directory`, read as text.
pub fn every_file_under(directory: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return String::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                every_file_under(&path)
            } else {
                String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).into_owned()
            }
        })
        .collect()
}

/// Call `tool_name` with `tool_arguments` through `rmcp`'s client over the
/// local API socket — one connection per call, as a one-shot client makes it.
pub fn call_a_tool_over_the_local_api(
    local_api_socket_path: &Path,
    tool_name: &str,
    tool_arguments: serde_json::Value,
) -> LocalApiToolCallAnswer {
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime for the MCP client");
    tokio_runtime.block_on(async {
        let local_api_mcp_uri = "http://localhost/mcp";
        let transport = StreamableHttpClientTransport::with_client(
            UnixSocketHttpClient::new(
                local_api_socket_path.to_str().expect("a UTF-8 socket path"),
                local_api_mcp_uri,
            ),
            StreamableHttpClientTransportConfig::with_uri(local_api_mcp_uri),
        );
        let client = ()
            .serve_with_lifecycle(
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the runtime answers `server/discover`");
        let request = CallToolRequestParams::new(tool_name.to_owned()).with_arguments(
            tool_arguments
                .as_object()
                .cloned()
                .expect("the tool's arguments are an object"),
        );
        let tool_result = client
            .call_tool(request)
            .await
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal}"));
        let _ = client.cancel().await;
        let tool_result = serde_json::to_value(tool_result).expect("a tool result serializes");
        LocalApiToolCallAnswer {
            is_error: tool_result["isError"].as_bool().unwrap_or(false),
            text: tool_result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        }
    })
}

/// The ids of the re-loads `tatolabd` logged it queued at its start, from its
/// standard error once it serves.
pub fn the_reloads_queued_at_the_start(standard_error: &str) -> Vec<String> {
    let serving_line = standard_error
        .lines()
        .find(|line| line.contains(THE_RUNTIME_IS_SERVING_LOG_LINE))
        .unwrap_or_else(|| panic!("the runtime logged no serving line:\n{standard_error}"));
    let queued = serving_line
        .split_once('(')
        .and_then(|(_, after)| after.split_once(')'))
        .map(|(queued, _)| queued)
        .unwrap_or_else(|| panic!("the serving line names no re-load queued: {serving_line}"));
    queued
        .split(", ")
        .filter(|action_id| !action_id.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How long a stream action queued over the local API takes to end.
pub const A_STREAM_ACTION_ENDS_WITHIN: Duration = Duration::from_secs(120);

/// `describe_stream_action`'s answer for `action_id` once its `state` is `done`
/// or `failed`.
pub fn the_stream_action_once_ended(
    local_api_socket_path: &Path,
    action_id: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + A_STREAM_ACTION_ENDS_WITHIN;
    loop {
        let described = call_a_tool_over_the_local_api(
            local_api_socket_path,
            "describe_stream_action",
            serde_json::json!({"action": action_id}),
        )
        .json();
        if described["state"] == "done" || described["state"] == "failed" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "the stream action `{action_id}` did not end within \
             {A_STREAM_ACTION_ENDS_WITHIN:?}: {described}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The mode bits of `path`.
pub fn the_mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .unwrap_or_else(|missing| panic!("{} has no metadata: {missing}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}
