// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Boot-path integration tests for the bare `streamlib-runtime` binary.
//!
//! These exercise the real binary (`CARGO_BIN_EXE_streamlib-runtime`) end
//! to end: it must boot as bare engine substrate with the statically-linked
//! API server registered in-process, serve `/health` against an empty
//! graph, and stream runtime events over `/ws/events` — with the archaic
//! `--plugin` / `--plugin-dir` loader gone.
//!
//! The API server is a host, not a dlopen'd plugin: `streamlib-runtime`
//! links `streamlib-api-server` as an `rlib` and calls
//! `PROCESSOR_REGISTRY.register::<ApiServerProcessor::Processor>()` at boot,
//! so there is no load-time cdylib build. Booting still initializes the
//! full runtime (GPU init included) and the api-server binds a real socket,
//! so this is a local integration test — the workspace CI runs
//! `cargo test --lib`, which does not pick up `tests/`.

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

/// Issue a bare HTTP/1.1 POST with a JSON body and return the numeric
/// status code. Raw TCP keeps the test dependency-free.
fn http_post_json(port: u16, path: &str, body: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body,
    );
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

/// First index of `needle` within `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A minimal blocking WebSocket client over raw TCP — just enough to
/// complete the RFC 6455 upgrade handshake and read server-sent frames.
/// Server→client frames are unmasked, so no masking key handling is needed
/// on the read path and no crypto dep is pulled in. Matches the raw-HTTP
/// idiom of the helpers above.
struct WsClient {
    stream: TcpStream,
    /// Bytes read past the HTTP handshake response that belong to the frame
    /// stream but haven't been consumed yet.
    buffered: Vec<u8>,
}

impl WsClient {
    /// Perform the HTTP Upgrade handshake on `path` and return a client
    /// positioned at the start of the WebSocket frame stream.
    fn connect(port: u16, path: &str) -> Option<WsClient> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        // Short read timeout so `read` returns promptly; the frame reader
        // loops against a caller-supplied deadline between reads.
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .ok()?;
        // Sec-WebSocket-Key is any base64-encoded 16-byte nonce; RFC 6455's
        // own sample value is fine — we don't validate the server's Accept.
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).ok()?;

        // Read until the response header terminator, keeping any frame bytes
        // that arrived in the same read.
        let mut buf = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let header_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut chunk = [0u8; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return None,
            }
        };

        let head = String::from_utf8_lossy(&buf[..header_end]);
        if !head.starts_with("HTTP/1.1 101") {
            return None;
        }

        Some(WsClient {
            stream,
            buffered: buf[header_end..].to_vec(),
        })
    }

    /// Ensure `buffered` holds at least `n` bytes, reading more from the
    /// socket until it does or `deadline` passes.
    fn ensure(&mut self, n: usize, deadline: Instant) -> bool {
        while self.buffered.len() < n {
            if Instant::now() >= deadline {
                return false;
            }
            let mut chunk = [0u8; 2048];
            match self.stream.read(&mut chunk) {
                Ok(0) => return false,
                Ok(m) => self.buffered.extend_from_slice(&chunk[..m]),
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// Read one WebSocket frame, returning `(opcode, payload)`.
    fn read_frame(&mut self, deadline: Instant) -> Option<(u8, Vec<u8>)> {
        if !self.ensure(2, deadline) {
            return None;
        }
        let opcode = self.buffered[0] & 0x0F;
        let masked = self.buffered[1] & 0x80 != 0;
        let len7 = (self.buffered[1] & 0x7F) as usize;

        let (header_len, payload_len) = if len7 < 126 {
            (2usize, len7)
        } else if len7 == 126 {
            if !self.ensure(4, deadline) {
                return None;
            }
            (
                4usize,
                u16::from_be_bytes([self.buffered[2], self.buffered[3]]) as usize,
            )
        } else {
            if !self.ensure(10, deadline) {
                return None;
            }
            let mut len_bytes = [0u8; 8];
            len_bytes.copy_from_slice(&self.buffered[2..10]);
            (10usize, u64::from_be_bytes(len_bytes) as usize)
        };

        let mask_len = if masked { 4 } else { 0 };
        let total = header_len + mask_len + payload_len;
        if !self.ensure(total, deadline) {
            return None;
        }

        let mut payload = self.buffered[header_len + mask_len..total].to_vec();
        if masked {
            let mask = [
                self.buffered[header_len],
                self.buffered[header_len + 1],
                self.buffered[header_len + 2],
                self.buffered[header_len + 3],
            ];
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        self.buffered.drain(..total);
        Some((opcode, payload))
    }

    /// Read frames until a text frame arrives (skipping ping / pong), or the
    /// deadline passes / the peer closes. Returns the decoded text payload.
    fn read_text(&mut self, deadline: Instant) -> Option<String> {
        loop {
            let (opcode, payload) = self.read_frame(deadline)?;
            match opcode {
                0x1 => return String::from_utf8(payload).ok(),
                0x8 => return None, // close
                _ => continue,      // ping / pong / continuation — keep reading
            }
        }
    }
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

    // The api-server is statically linked, so there is no load-time cdylib
    // build — boot is process start + GPU init + socket bind. Allow a
    // generous window for GPU init before the control plane is reachable.
    let served = wait_for_health(base_port, Duration::from_secs(60));

    let _ = std::fs::remove_dir_all(&temp_home);
    assert!(
        served.is_some(),
        "bare runtime should boot with the in-process api-server and serve /health"
    );
}

#[test]
fn websocket_streams_runtime_events_end_to_end() {
    let base_port = free_port();
    let temp_home = std::env::temp_dir().join(format!("streamlib-runtime-ws-{base_port}"));
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

    let port =
        wait_for_health(base_port, Duration::from_secs(60)).expect("runtime should serve /health");

    let mut ws =
        WsClient::connect(port, "/ws/events").expect("WebSocket upgrade on /ws/events should 101");

    // The api-server's WS handler calls `PUBSUB.subscribe(topics::ALL, ...)`.
    // Because the api-server is statically linked into the runtime, that
    // subscribe runs on the host's initialized PUBSUB. When the api-server
    // was a dlopen'd cdylib its own PUBSUB was never `init()`ed, so
    // `subscribe` buffered into `pending_subscriptions` forever and no WS
    // frame ever arrived (only `publish` was ABI-bridged) — this asserts a
    // runtime event now reaches a WebSocket client end to end.
    //
    // Trigger: adding an unregistered processor type is side-effect-free —
    // the graph mutation publishes RuntimeWillAddProcessor /
    // RuntimeDidAddProcessor to topics::RUNTIME_GLOBAL (fanned out to
    // topics::ALL) and returns 422, leaving only an Error-state node. The
    // subscriber thread takes a beat to come up and iceoryx2 does not replay
    // pre-subscription samples, so re-trigger within a deadline until a
    // frame lands.
    let probe = r#"{"processor_type":{"org":"tatolab","package":"regression-probe","type":"WsProbe","version":{"major":1,"minor":0,"patch":0}},"config":{}}"#;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut received: Option<String> = None;
    while received.is_none() && Instant::now() < deadline {
        let _ = http_post_json(port, "/api/processor", probe);
        received = ws.read_text(Instant::now() + Duration::from_secs(2));
    }

    let _ = std::fs::remove_dir_all(&temp_home);

    let event_json = received.expect(
        "a runtime event must reach the WebSocket client — the in-process \
         api-server subscribes on the host's initialized PUBSUB; a dlopen'd \
         plugin's bus is never init()ed, so subscribe would buffer forever",
    );
    // The frame is a serialized `Event`. Assert it decodes as JSON without
    // pinning a specific variant, so the lock survives event-schema churn.
    let parsed: serde_json::Value =
        serde_json::from_str(&event_json).expect("WS frame should be JSON");
    assert!(
        parsed.is_object() || parsed.is_string(),
        "WS event frame should decode as a JSON value; got: {event_json}"
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
