// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Boot-path integration tests for the bare `streamlib-runtime` binary.
//!
//! These exercise the real binary (`CARGO_BIN_EXE_streamlib-runtime`) end
//! to end: it must boot as bare engine substrate, load the API server
//! through the all-dynamic module loader, and serve `/health` against an
//! empty graph — with the archaic `--plugin` / `--plugin-dir` loader gone.
//!
//! The boot test builds the `@tatolab/api-server` cdylib via the injected
//! orchestrator and starts a full runtime (GPU init included), so it is a
//! local integration test (the workspace CI runs `cargo test --lib`,
//! which does not pick up `tests/`). It relies on the runtime resolving
//! `packages/` from the test binary's own location (`target/<profile>/`
//! walks up to the workspace root), exercising the real resolver.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kills the spawned runtime when the test ends (pass or panic).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Grab an ephemeral port the OS reports free, then release it. The
/// api-server binds the requested port and increments on collision, so
/// callers poll a small window above this value.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Issue a bare HTTP/1.1 GET and return the numeric status code, or `None`
/// if the connection/parse failed. Raw TCP keeps the test dependency-free.
fn http_get_status(port: u16, path: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let response = String::from_utf8_lossy(&buf);
    let status_line = response.lines().next()?;
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// Poll `/health` across the api-server's bind-retry window until it
/// returns 200 or the deadline passes. Returns the port that answered.
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

#[test]
fn bare_runtime_boots_and_serves_health() {
    let base_port = free_port();
    let temp_home = std::env::temp_dir().join(format!("streamlib-runtime-boot-{base_port}"));
    let _ = std::fs::remove_dir_all(&temp_home);

    let child = Command::new(env!("CARGO_BIN_EXE_streamlib-runtime"))
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(base_port.to_string())
        .env("STREAMLIB_HOME", &temp_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn streamlib-runtime");
    let _guard = ChildGuard(child);

    // First boot builds the api-server cdylib via the orchestrator, so
    // allow a generous window before the control plane is reachable.
    let served = wait_for_health(base_port, Duration::from_secs(180));

    let _ = std::fs::remove_dir_all(&temp_home);
    assert!(
        served.is_some(),
        "bare runtime should boot, load the api-server module, and serve /health"
    );
}

#[test]
fn rejects_removed_plugin_args() {
    for arg in ["--plugin", "--plugin-dir"] {
        let output = Command::new(env!("CARGO_BIN_EXE_streamlib-runtime"))
            .arg(arg)
            .output()
            .expect("spawn streamlib-runtime");

        assert!(
            !output.status.success(),
            "the archaic `{arg}` argument must be rejected, not accepted"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unexpected argument") && stderr.contains(arg),
            "`{arg}` should produce a clap unknown-argument error; stderr was:\n{stderr}"
        );
    }
}
