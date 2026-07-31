// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Boot-path integration tests for `streamlib run` / `streamlib dev`.
//!
//! These drive the real `streamlib` binary end to end: it must resolve the
//! app's entry file, boot a node carrying the statically-linked api-server,
//! publish a node-registry entry that `streamlib nodes` can discover, and
//! remove that entry on clean teardown.
//!
//! Booting initializes the full runtime (GPU init included) and binds a real
//! socket, so the boot tests are local integration tests — the workspace CI
//! runs `cargo test --lib`, which does not pick up `tests/`. The
//! entry-resolution failure tests below need no rig: the binary exits before
//! it builds a runtime.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod cli_integration_support;
use cli_integration_support::STREAMLIB_BINARY_PATH;

/// Kills the spawned node when the test ends (pass or panic).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A temp dir tree that removes itself when the test ends.
struct TempTreeRemovedOnDrop(PathBuf);

impl TempTreeRemovedOnDrop {
    fn new(name: &str, discriminator: u16) -> Self {
        let path = std::env::temp_dir().join(format!("streamlib-run-boot-{name}-{discriminator}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp tree");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTreeRemovedOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

/// Issue a bare HTTP/1.1 GET and return the numeric status code, or `None` if
/// the connection/parse failed. Raw TCP keeps the test dependency-free.
fn http_get_status(port: u16, path: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let response = String::from_utf8_lossy(&buf);
    response
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
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
) -> Child {
    Command::new(STREAMLIB_BINARY_PATH)
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
        .expect("spawn streamlib")
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

/// Boot a node with `verb`, assert it is discoverable, then interrupt it and
/// assert the registry entry is gone.
fn assert_verb_boots_registers_and_tears_down(verb: &str) {
    let port = free_port();
    let app = TempTreeRemovedOnDrop::new(&format!("{verb}-app"), port);
    let home = TempTreeRemovedOnDrop::new(&format!("{verb}-home"), port);
    let xdg = TempTreeRemovedOnDrop::new(&format!("{verb}-xdg"), port);
    write_minimal_app(app.path(), "app.py");

    let child = spawn_app_node(verb, app.path(), port, home.path(), xdg.path(), &[]);
    let pid = child.id();
    let mut guard = ChildGuard(child);

    // Boot is process start + GPU init + socket bind; allow a generous window.
    let served = wait_for_health(port, Duration::from_secs(60));
    assert!(
        served.is_some(),
        "`streamlib {verb}` should boot a node serving /health"
    );

    let entry = wait_for_sole_registry_entry(xdg.path(), Duration::from_secs(10))
        .unwrap_or_else(|| panic!("`streamlib {verb}` should publish one node-registry entry"));
    assert_eq!(
        entry["pid"].as_u64(),
        Some(u64::from(pid)),
        "the entry must name the hosting process"
    );
    assert_eq!(
        entry["control_url"].as_str(),
        Some(format!("http://127.0.0.1:{}", served.unwrap()).as_str()),
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

    interrupt(pid);
    let exited = guard.0.wait().expect("wait for the node to exit");
    assert!(
        exited.success(),
        "`streamlib {verb}` should exit cleanly on SIGINT, got {exited}"
    );

    assert!(
        registry_entry_paths(xdg.path()).is_empty(),
        "clean teardown must remove the node-registry entry"
    );
}

#[test]
#[ignore = "boots a full runtime (GPU init + socket bind) — needs the local rig"]
fn run_boots_a_discoverable_node_and_tears_it_down_on_interrupt() {
    assert_verb_boots_registers_and_tears_down("run");
}

#[test]
#[ignore = "boots a full runtime (GPU init + socket bind) — needs the local rig"]
fn dev_shares_the_same_resolution_and_boot_path_as_run() {
    assert_verb_boots_registers_and_tears_down("dev");
}

#[test]
#[ignore = "boots a full runtime (GPU init + socket bind) — needs the local rig"]
fn an_explicit_entry_file_boots_a_node_when_the_convention_is_absent() {
    let port = free_port();
    let app = TempTreeRemovedOnDrop::new("explicit-app", port);
    let home = TempTreeRemovedOnDrop::new("explicit-home", port);
    let xdg = TempTreeRemovedOnDrop::new("explicit-xdg", port);
    write_minimal_app(app.path(), "other.py");

    let child = spawn_app_node(
        "run",
        app.path(),
        port,
        home.path(),
        xdg.path(),
        &["-f", "other.py"],
    );
    let pid = child.id();
    let mut guard = ChildGuard(child);

    assert!(
        wait_for_health(port, Duration::from_secs(60)).is_some(),
        "`-f other.py` should boot a node even with no app.py present"
    );

    interrupt(pid);
    let _ = guard.0.wait();
}

#[test]
fn a_missing_conventional_entry_fails_before_any_runtime_is_built() {
    let app = TempTreeRemovedOnDrop::new("no-entry", free_port());

    for verb in ["run", "dev"] {
        let output = Command::new(STREAMLIB_BINARY_PATH)
            .arg(verb)
            .arg("--dir")
            .arg(app.path())
            .output()
            .expect("spawn streamlib");

        assert!(
            !output.status.success(),
            "`streamlib {verb}` in an entry-less dir must fail"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("app.py"),
            "the error must name the convention; stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains("never searches parent directories"),
            "the error must state the no-walk-up rule; stderr was:\n{stderr}"
        );
        assert!(
            stderr.contains(&app.path().display().to_string()),
            "the error must name the anchor it searched; stderr was:\n{stderr}"
        );
    }
}

#[test]
fn a_parent_directorys_entry_is_never_adopted() {
    let app = TempTreeRemovedOnDrop::new("walk-up", free_port());
    write_minimal_app(app.path(), "app.py");
    let nested = app.path().join("nested");
    std::fs::create_dir_all(&nested).expect("create nested dir");

    let output = Command::new(STREAMLIB_BINARY_PATH)
        .arg("run")
        .arg("--dir")
        .arg(&nested)
        .output()
        .expect("spawn streamlib");

    assert!(
        !output.status.success(),
        "a parent's app.py must not be adopted by a nested anchor"
    );
}

#[test]
fn a_missing_explicit_entry_names_the_path_it_tried() {
    let app = TempTreeRemovedOnDrop::new("missing-explicit", free_port());

    let output = Command::new(STREAMLIB_BINARY_PATH)
        .arg("run")
        .arg("--dir")
        .arg(app.path())
        .arg("-f")
        .arg("gone.py")
        .output()
        .expect("spawn streamlib");

    assert!(
        !output.status.success(),
        "a nonexistent `-f` target must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gone.py"),
        "the error must name the path it tried; stderr was:\n{stderr}"
    );
}
