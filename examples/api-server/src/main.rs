// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minimal control-plane readiness probe.
//!
//! The api-server is the runtime's HTTP + WebSocket command-and-control plane.
//! It is not a loadable plugin: `streamlib-runtime` statically links it and
//! serves it in-process, so an app reaches the control plane by running the
//! runtime and hitting its endpoints — there is no module to `add_module` and
//! nothing to `dlopen`. This example is the smallest form of that: it connects
//! to an already-running `streamlib-runtime` and probes `GET /health` and
//! `GET /api/registry`, printing what the control plane reports.
//!
//! Start the runtime first (it serves on `http://127.0.0.1:9000` by default),
//! then run this probe:
//!
//!     <checkout>/target/debug/streamlib-runtime      # in one terminal
//!     cargo run                                       # in this directory
//!     cargo run -- 127.0.0.1:9000                     # or an explicit host:port
//!
//! For a full REST + WebSocket walk of every control endpoint, see the
//! `api-server-demo` example.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn main() {
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9000".to_string());

    println!("Probing the streamlib-runtime control plane at http://{target}");
    println!("(start it with `<checkout>/target/debug/streamlib-runtime`)\n");

    for path in ["/health", "/api/registry"] {
        match http_get(&target, path) {
            Ok(body) => println!("GET {path}\n{body}\n"),
            Err(err) => {
                println!("GET {path} failed: {err}");
                println!("Is streamlib-runtime running and serving at {target}?");
                std::process::exit(1);
            }
        }
    }
}

/// Issue a dependency-free HTTP/1.1 GET and return the response body. Raw TCP
/// keeps the probe dependency-free, mirroring the idiom the runtime's own boot
/// integration test uses.
fn http_get(target: &str, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(target)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response);
    // Split the response headers from the body at the blank line; return the
    // body (the JSON the control plane emitted).
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&text);
    Ok(body.trim().to_string())
}
