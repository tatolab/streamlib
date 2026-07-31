// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Boot-path integration tests for `streamlib run` / `streamlib dev`.
//!
//! These drive the real `streamlib` binary end to end: it must resolve the
//! app's entry file, boot a node carrying the statically-linked api-server,
//! publish a node-registry entry that `streamlib nodes` can discover, keep the
//! JSONL log the observability contract promises, and remove the entry on clean
//! teardown.
//!
//! Booting initializes the full runtime (GPU init included) and binds a real
//! socket, matching `runtime/streamlib-runtime/tests/boot.rs`, which boots the
//! same runtime and is likewise not gated. CI runs `cargo test --lib` and so
//! picks up neither file.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod cli_integration_support;
use cli_integration_support::STREAMLIB_BINARY_PATH;

/// How long a resolution-failure invocation may take before the test treats it
/// as wedged. These exit before any runtime is built, so this is generous.
const RESOLUTION_FAILURE_DEADLINE: Duration = Duration::from_secs(30);

/// Kills the spawned node when the test ends (pass or panic).
struct SpawnedNodeKilledOnDrop(Child);

impl SpawnedNodeKilledOnDrop {
    fn process_id(&self) -> u32 {
        self.0.id()
    }

    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        self.0.wait().expect("wait for the node to exit")
    }
}

impl Drop for SpawnedNodeKilledOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Grab an ephemeral port the OS reports free, then release it. The api-server
/// binds the requested port and increments on collision, so callers poll a
/// small window above this value.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// The HTTP status of a GET against the node, or `None` when the connection
/// failed. `ureq` is already a dependency of this crate (the `mcp --attach`
/// client), and its timeouts keep a silent socket from wedging a poll loop.
fn http_get_status(port: u16, path: &str) -> Option<u16> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build();
    match agent.get(&format!("http://127.0.0.1:{port}{path}")).call() {
        Ok(response) => Some(response.status()),
        Err(ureq::Error::Status(code, _)) => Some(code),
        Err(_) => None,
    }
}

/// Poll `/health` across the api-server's bind-retry window until it returns
/// 200 or the deadline passes. Returns the port that answered.
fn wait_for_health(base_port: u16, timeout: Duration) -> Option<u16> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for port in base_port..base_port + 10 {
            if http_get_status(port, "/health") == Some(200) {
                return Some(port);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Every `*.json` node-registry entry under an isolated `XDG_RUNTIME_DIR`.
///
/// The layout is re-derived rather than read from
/// `node_registry::registry_dir()` on purpose: that resolver reads *this*
/// process's `XDG_RUNTIME_DIR`, not the spawned child's, so calling it would
/// force a process-global env mutation and break isolation under parallelism.
fn registry_entry_paths(xdg_runtime_dir: &Path) -> Vec<PathBuf> {
    let nodes_dir = xdg_runtime_dir.join("streamlib").join("nodes");
    let Ok(entries) = std::fs::read_dir(&nodes_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

/// Poll until the isolated registry holds exactly one entry, or the deadline
/// passes. Returns the decoded entry body.
fn wait_for_sole_registry_entry(
    xdg_runtime_dir: &Path,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let paths = registry_entry_paths(xdg_runtime_dir);
        if paths.len() == 1
            && let Ok(bytes) = std::fs::read(&paths[0])
            && let Ok(entry) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            return Some(entry);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

/// Write the smallest app the entry convention accepts: an `app.py` carrying a
/// `setup(rt)`. Harness execution of that function lands with the language
/// harnesses; boot only requires the file to resolve.
fn write_minimal_app(app_dir: &Path, entry_file_name: &str) {
    std::fs::write(app_dir.join(entry_file_name), "def setup(rt):\n    pass\n")
        .expect("write app entry");
}

/// Spawn `streamlib <verb>` against an isolated home + node registry.
fn spawn_app_node(
    verb: &str,
    app_dir: &Path,
    port: u16,
    streamlib_home: &Path,
    xdg_runtime_dir: &Path,
    extra_args: &[&str],
) -> SpawnedNodeKilledOnDrop {
    let child = Command::new(STREAMLIB_BINARY_PATH)
        .arg(verb)
        .arg("--dir")
        .arg(app_dir)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .args(extra_args)
        .env("STREAMLIB_HOME", streamlib_home)
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn streamlib");
    SpawnedNodeKilledOnDrop(child)
}

/// Run a control verb against the isolated registry and return its output.
fn run_control_verb(args: &[&str], xdg_runtime_dir: &Path) -> std::process::Output {
    Command::new(STREAMLIB_BINARY_PATH)
        .args(args)
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .output()
        .expect("spawn streamlib control verb")
}

/// Send SIGINT to `pid` — the signal `wait_for_signal` installs a handler for.
fn interrupt(pid: u32) {
    let status = Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("spawn kill");
    assert!(status.success(), "SIGINT delivery to pid {pid} failed");
}

/// Run `streamlib` with `args` and require it to exit within
/// [`RESOLUTION_FAILURE_DEADLINE`], returning its exit status and stderr.
///
/// A plain `Command::output()` would block forever on the very regression these
/// callers guard: if entry resolution ever starts walking up, the binary boots
/// a node and waits for a signal instead of exiting. Killing at the deadline
/// turns that into a red test rather than a wedged suite.
///
/// The isolated home / registry and the ephemeral port matter for exactly that
/// failing case: the node this must never boot would otherwise land in the
/// developer's real registry on the default port, and the deadline kill is
/// SIGKILL, which skips the teardown that would have removed its entry.
fn run_expecting_prompt_exit(args: &[&str]) -> (std::process::ExitStatus, String) {
    let home = tempfile::tempdir().expect("create home dir");
    let xdg = tempfile::tempdir().expect("create xdg dir");
    let port = free_port().to_string();

    let mut child = Command::new(STREAMLIB_BINARY_PATH)
        .args(args)
        .arg("--port")
        .arg(&port)
        .env("STREAMLIB_HOME", home.path())
        .env("XDG_RUNTIME_DIR", xdg.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn streamlib");

    let deadline = Instant::now() + RESOLUTION_FAILURE_DEADLINE;
    let status = loop {
        match child.try_wait().expect("poll streamlib") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`streamlib {}` did not exit within {RESOLUTION_FAILURE_DEADLINE:?} — \
                     entry resolution booted a node instead of failing",
                    args.join(" "),
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (status, stderr)
}

/// Boot a node with `verb`, assert it is a first-class runtime host, then
/// interrupt it and assert the registry entry is gone.
fn assert_verb_boots_registers_and_tears_down(verb: &str) {
    let app = tempfile::tempdir().expect("create app dir");
    let home = tempfile::tempdir().expect("create home dir");
    let xdg = tempfile::tempdir().expect("create xdg dir");
    let port = free_port();
    write_minimal_app(app.path(), "app.py");

    let mut node = spawn_app_node(verb, app.path(), port, home.path(), xdg.path(), &[]);
    let pid = node.process_id();

    // Boot is process start + GPU init + socket bind; allow a generous window.
    let served_port = wait_for_health(port, Duration::from_secs(60))
        .unwrap_or_else(|| panic!("`streamlib {verb}` should boot a node serving /health"));

    let entry = wait_for_sole_registry_entry(xdg.path(), Duration::from_secs(10))
        .unwrap_or_else(|| panic!("`streamlib {verb}` should publish one node-registry entry"));
    assert_eq!(
        entry["pid"].as_u64(),
        Some(u64::from(pid)),
        "the entry must name the hosting process"
    );
    assert_eq!(
        entry["control_url"].as_str(),
        Some(format!("http://127.0.0.1:{served_port}").as_str()),
        "the entry must carry the reachable control URL"
    );

    let runtime_id = entry["runtime_id"]
        .as_str()
        .expect("the entry must carry a runtime_id");

    let nodes = run_control_verb(&["nodes"], xdg.path());
    assert!(nodes.status.success(), "`streamlib nodes` should succeed");
    let listing = String::from_utf8_lossy(&nodes.stdout);
    assert!(
        listing.contains(runtime_id),
        "`streamlib nodes` should list the booted node `{runtime_id}`; stdout was:\n{listing}"
    );

    let graph = run_control_verb(&["graph", "--node", runtime_id], xdg.path());
    assert!(
        graph.status.success(),
        "`streamlib graph` should round-trip against the booted node; stderr was:\n{}",
        String::from_utf8_lossy(&graph.stderr)
    );

    assert_hosted_node_keeps_its_jsonl_log(&graph.stdout, home.path(), verb);

    interrupt(pid);
    let exited = node.wait_for_exit();
    assert!(
        exited.success(),
        "`streamlib {verb}` should exit cleanly on SIGINT, got {exited}"
    );

    assert!(
        registry_entry_paths(xdg.path()).is_empty(),
        "clean teardown must remove the node-registry entry"
    );
}

/// A CLI-hosted node must be as observable as a `streamlib-runtime`-hosted one:
/// the api-server carries a `log_path` and that JSONL file exists.
///
/// `logging::init` is first-caller-wins, so a CLI that initialized its own
/// short-lived logging before building the `Runner` would silently demote the
/// runtime's init to a noop guard and leave `jsonl_log_path()` empty. This is
/// the lock on that regression.
fn assert_hosted_node_keeps_its_jsonl_log(graph_stdout: &[u8], streamlib_home: &Path, verb: &str) {
    let graph: serde_json::Value =
        serde_json::from_slice(graph_stdout).expect("`streamlib graph` should emit JSON");

    let log_path = find_api_server_log_path(&graph).unwrap_or_else(|| {
        panic!(
            "`streamlib {verb}` must pass the runtime's JSONL log path to the api-server; \
             graph was:\n{graph:#}"
        )
    });

    assert!(
        Path::new(&log_path).is_file(),
        "the api-server's log_path `{log_path}` must exist on disk"
    );
    assert!(
        Path::new(&log_path).starts_with(streamlib_home),
        "the JSONL log `{log_path}` must live under the node's STREAMLIB_HOME"
    );

    // The resolution line is emitted after the Runner exists precisely so it
    // lands here; moving it back before `Runner::with_auto_build()` silently
    // drops it on the floor, and this is what notices.
    let log_body = std::fs::read_to_string(&log_path).expect("read the node's JSONL log");
    assert!(
        log_body.contains("Resolved app entry"),
        "the entry-resolution line must reach the JSONL log — it is emitted with no subscriber \
         installed if it precedes the Runner; log was:\n{log_body}"
    );
}

/// The `log_path` config value of the api-server processor in a graph snapshot.
///
/// Anchored on the object that also carries the api-server's `host` + `port` so
/// an unrelated `log_path` elsewhere in a future graph cannot satisfy this.
fn find_api_server_log_path(graph: &serde_json::Value) -> Option<String> {
    fn walk(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Object(map) => {
                let is_api_server_config = map.contains_key("host") && map.contains_key("port");
                if is_api_server_config
                    && let Some(serde_json::Value::String(log_path)) = map.get("log_path")
                {
                    return Some(log_path.clone());
                }
                map.values().find_map(walk)
            }
            serde_json::Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    walk(graph)
}

#[test]
fn run_boots_a_discoverable_node_and_tears_it_down_on_interrupt() {
    assert_verb_boots_registers_and_tears_down("run");
}

#[test]
fn dev_shares_the_same_resolution_and_boot_path_as_run() {
    assert_verb_boots_registers_and_tears_down("dev");
}

#[test]
fn an_explicit_entry_file_boots_a_node_when_the_convention_is_absent() {
    let app = tempfile::tempdir().expect("create app dir");
    let home = tempfile::tempdir().expect("create home dir");
    let xdg = tempfile::tempdir().expect("create xdg dir");
    let port = free_port();
    write_minimal_app(app.path(), "other.py");

    let mut node = spawn_app_node(
        "run",
        app.path(),
        port,
        home.path(),
        xdg.path(),
        &["-f", "other.py"],
    );
    let pid = node.process_id();

    assert!(
        wait_for_health(port, Duration::from_secs(60)).is_some(),
        "`-f other.py` should boot a node even with no app.py present"
    );

    interrupt(pid);
    node.wait_for_exit();
}

#[test]
fn a_missing_conventional_entry_fails_before_any_runtime_is_built() {
    let app = tempfile::tempdir().expect("create app dir");
    let anchor = app.path().display().to_string();

    for verb in ["run", "dev"] {
        let (status, stderr) = run_expecting_prompt_exit(&[verb, "--dir", &anchor]);

        assert!(
            !status.success(),
            "`streamlib {verb}` in an entry-less dir must fail"
        );
        assert!(
            stderr.contains("app.py"),
            "the error must name the convention; stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains("never searches parent directories"),
            "the error must state the no-walk-up rule; stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains(&anchor),
            "the error must name the anchor it searched; stderr was:\n{stderr}"
        );
    }
}

#[test]
fn a_parent_directorys_entry_is_never_adopted() {
    let app = tempfile::tempdir().expect("create app dir");
    write_minimal_app(app.path(), "app.py");
    let nested = app.path().join("nested");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let nested_anchor = nested.display().to_string();

    let (status, stderr) = run_expecting_prompt_exit(&["run", "--dir", &nested_anchor]);

    assert!(
        !status.success(),
        "a parent's app.py must not be adopted by a nested anchor"
    );
    assert!(
        stderr.contains("never searches parent directories"),
        "the failure must be the no-walk-up rule, not an unrelated error; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains(&nested_anchor),
        "the error must name the nested anchor, not the parent; stderr was:\n{stderr}"
    );
}

#[test]
fn a_missing_explicit_entry_names_the_path_it_tried() {
    let app = tempfile::tempdir().expect("create app dir");
    let anchor = app.path().display().to_string();

    let (status, stderr) = run_expecting_prompt_exit(&["run", "--dir", &anchor, "-f", "gone.py"]);

    assert!(!status.success(), "a nonexistent `-f` target must fail");
    assert!(
        stderr.contains("gone.py"),
        "the error must name the path it tried; stderr was:\n{stderr}"
    );
}
