// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib mcp` — speak the Model Context Protocol over stdio so any MCP host
//! (`claude mcp add streamlib -- streamlib mcp`) can spawn StreamLib's
//! agent-operable tools with zero port/daemon juggling.
//!
//! Two modes, both hosting the api-server's one transport-free MCP dispatch
//! ([`streamlib_api_server::serve_stdio_jsonrpc`]) — never a parallel MCP impl:
//!
//! - **Default (in-process):** build a fresh live [`Runner`] and serve its MCP
//!   over the process's stdio; the host gets an operable runtime with no setup.
//!   Torn down when the host closes stdin (EOF).
//! - **`--attach <url>`:** forward each stdio JSON-RPC line to a running
//!   runtime's `POST /mcp`, to operate an existing live pipeline; no local
//!   Runner is built.
//!
//! Auth is a no-op on the in-process path by construction (a local child
//! process, no bearer header); only `--attach` may forward a token
//! (`STREAMLIB_MCP_TOKEN`) to the remote endpoint.

use std::sync::Arc;

use anyhow::Result;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::runtime::{Runner, RuntimeOperations};

/// Run the `mcp` subcommand. `attach` selects the bridge-to-remote mode; its
/// absence is the in-process default.
pub async fn run(attach: Option<String>) -> Result<()> {
    match attach {
        Some(url) => attach_to_remote(url).await,
        None => serve_in_process().await,
    }
}

/// Build a live in-process runtime and serve MCP over stdio against it. The
/// runtime is started before the loop and stopped on stdin EOF (the host
/// closing the pipe). Needs the runtime rig (GPU/iceoryx2) — an MCP host spawns
/// this in the user's environment, so it has the rig.
async fn serve_in_process() -> Result<()> {
    let runner = Runner::with_auto_build()?;
    runner.start()?;

    let runtime: Arc<dyn RuntimeOperations> = runner.clone();
    let served = streamlib_api_server::serve_stdio_jsonrpc(
        runtime,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await;

    // Tear the runtime down regardless of how the loop ended, then surface any
    // transport error.
    if let Err(stop_error) = runner.stop() {
        tracing::warn!("runtime stop after MCP stdio EOF failed: {stop_error}");
    }
    served?;
    Ok(())
}

/// Bridge stdio ↔ a running runtime's `POST /mcp`: each inbound JSON-RPC line is
/// POSTed to the remote endpoint and the response body (if any) written back as
/// a line. Runs the whole blocking bridge on a blocking thread so the tokio
/// runtime is never parked on `ureq` I/O.
async fn attach_to_remote(url: String) -> Result<()> {
    tokio::task::spawn_blocking(move || attach_to_remote_blocking(&url)).await?
}

fn attach_to_remote_blocking(url: &str) -> Result<()> {
    let bearer_token = std::env::var("STREAMLIB_MCP_TOKEN").ok();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    bridge_stdio_to_remote(url, bearer_token.as_deref(), stdin.lock(), stdout.lock())
}

/// Forward each newline-delimited JSON-RPC line from `reader` to `{url}/mcp`,
/// writing each non-empty response body back to `writer` as a line. A 2xx with
/// an empty body (a notification's `202`) yields no output line; a non-2xx
/// surfaces an error. `bearer_token`, when set, rides as an `authorization:
/// Bearer` header. Generic over the byte transport so the CLI wires the process
/// stdio while a test drives an in-memory pipe — mirroring
/// [`streamlib_api_server::serve_stdio_jsonrpc`].
fn bridge_stdio_to_remote(
    url: &str,
    bearer_token: Option<&str>,
    reader: impl std::io::BufRead,
    mut writer: impl std::io::Write,
) -> Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // 2xx: a request answers `200` with the JSON-RPC envelope; a
        // notification answers `202` with an empty body → no response line.
        let body = super::control::post_mcp_request(url, bearer_token, &line)?;
        if !body.trim().is_empty() {
            writeln!(writer, "{body}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Hermetic tests for the `--attach` stdio→HTTP bridge: a local TCP server
    //! stands in for a running runtime's `POST /mcp`, so the forward loop,
    //! notification handling, bearer forwarding, and error surfacing are
    //! exercised without a live runtime. The bridge reads an in-memory pipe
    //! (not the process stdio) via the [`bridge_stdio_to_remote`] seam.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::*;

    /// One request as the mock server observed it.
    struct RecordedRequest {
        authorization: Option<String>,
        body: String,
    }

    /// A canned HTTP reply for one request.
    struct MockReply {
        status_line: &'static str,
        body: &'static str,
    }

    type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

    /// Spin a local HTTP server that answers `replies.len()` requests in order,
    /// recording each request's `authorization` header and body. Binds on port
    /// 0 and returns the resolved base URL, the recording handle, and the
    /// server thread's join handle.
    fn spawn_mock_mcp_server(
        replies: Vec<MockReply>,
    ) -> (String, RecordedRequests, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock mcp server");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let recorded: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
        let recorded_for_thread = recorded.clone();
        let handle = std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().expect("accept");
                let request = read_http_request(&stream);
                recorded_for_thread.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    reply.status_line,
                    reply.body.len(),
                    reply.body,
                );
                stream.write_all(response.as_bytes()).expect("write response");
                stream.flush().ok();
            }
        });
        (url, recorded, handle)
    }

    /// Parse one HTTP/1.1 request off `stream`: capture the `authorization`
    /// header, then read exactly `content-length` body bytes.
    fn read_http_request(stream: &TcpStream) -> RecordedRequest {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut request_line = String::new();
        reader.read_line(&mut request_line).expect("request line");

        let mut authorization = None;
        let mut content_length = 0usize;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("header line");
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').unwrap_or((header, ""));
            match name.trim().to_ascii_lowercase().as_str() {
                "authorization" => authorization = Some(value.trim().to_string()),
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).expect("read body");
        RecordedRequest {
            authorization,
            body: String::from_utf8(body).expect("utf8 body"),
        }
    }

    #[test]
    fn attach_bridge_round_trips_a_request_line() {
        let reply_body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let (url, recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "200 OK",
            body: reply_body,
        }]);

        let input: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        bridge_stdio_to_remote(&url, None, input, &mut output).expect("bridge");
        server.join().unwrap();

        assert_eq!(String::from_utf8(output).unwrap().trim(), reply_body);
        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].body.contains("\"method\":\"ping\""));
        assert_eq!(recorded[0].authorization, None);
    }

    #[test]
    fn attach_bridge_writes_no_line_for_a_notification_202() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "202 Accepted",
            body: "",
        }]);

        let input: &[u8] = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let mut output = Vec::new();
        bridge_stdio_to_remote(&url, None, input, &mut output).expect("bridge");
        server.join().unwrap();

        assert!(
            output.is_empty(),
            "a 202 empty body must yield no response line"
        );
    }

    #[test]
    fn attach_bridge_forwards_the_bearer_token() {
        let (url, recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "200 OK",
            body: "{}",
        }]);

        let input: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        bridge_stdio_to_remote(&url, Some("secret-token"), input, &mut output).expect("bridge");
        server.join().unwrap();

        assert_eq!(
            recorded.lock().unwrap()[0].authorization.as_deref(),
            Some("Bearer secret-token"),
            "STREAMLIB_MCP_TOKEN must ride as an authorization: Bearer header"
        );
    }

    #[test]
    fn attach_bridge_surfaces_a_non_2xx_as_an_error() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "500 Internal Server Error",
            body: "boom",
        }]);

        let input: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        let result = bridge_stdio_to_remote(&url, None, input, &mut output);
        server.join().unwrap();

        let error = result.expect_err("a non-2xx must surface as an error");
        assert!(
            error.to_string().contains("500"),
            "error must report the HTTP status; got: {error}"
        );
    }
}
