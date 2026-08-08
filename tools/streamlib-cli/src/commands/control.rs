// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib graph | tap | logs | shutdown` — thin JSON-RPC control clients
//! over a running node's `POST {url}/mcp`.
//!
//! Each verb marshals its args into a `tools/call` for one api-server MCP tool
//! ([`streamlib_api_server`]'s `tool_definitions`) and POSTs it over the same
//! `ureq` seam the `mcp --attach` bridge uses ([`post_mcp_request`], shared
//! with [`super::mcp`]). There is no local runtime and no second dispatch: the
//! verbs are exactly the api-server's tool catalog, and the arg shapes mirror
//! each tool's `inputSchema` 1:1. The catalog is observation-shaped, so there
//! is no verb here that mutates a graph.
//!
//! The optional `STREAMLIB_MCP_TOKEN` rides as an `authorization: Bearer`
//! header, matching the `--attach` bridge. Result handling covers four
//! channels: a non-2xx HTTP status, a top-level JSON-RPC `error` (returned
//! inside an HTTP 200), a tool-level `result.isError`, and success — the first
//! three exit non-zero with the error text, the last prints the tool result's
//! already-pretty text content.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// Resolve the control-plane base URL a verb targets from the optional `--url`
/// and `--node` flags, consulting the node registry when neither pins a URL:
///
/// - `--url` wins outright (an explicit endpoint, registered or not).
/// - else `--node <runtime_id>` resolves that node's `control_url` from the
///   registry (error if no such entry).
/// - else the SOLE live node's `control_url` (zero-ceremony single-node default).
/// - else — zero live nodes, or more than one and neither flag given — an error
///   that lists the live nodes so the caller can pick one with `--node`.
pub fn resolve_control_url(url: Option<String>, node: Option<String>) -> Result<String> {
    if let Some(url) = url {
        return Ok(url);
    }
    if let Some(node) = node {
        return match streamlib_api_server::node_registry::read_entry(&node)? {
            Some(entry) => Ok(entry.control_url),
            None => {
                let live = super::nodes::live_nodes()?;
                bail!(
                    "no registered node with runtime_id `{node}`.{}",
                    render_node_hint(&live)
                );
            }
        };
    }

    let mut live = super::nodes::live_nodes()?;
    match live.len() {
        1 => Ok(live.remove(0).control_url),
        0 => bail!(
            "no live StreamLib nodes found. Start a node that hosts an ApiServer \
             control plane, or pass `--url <control-plane-url>` explicitly."
        ),
        _ => bail!(
            "{} live nodes found; disambiguate with `--node <runtime_id>` or \
             `--url <control-plane-url>`.{}",
            live.len(),
            render_node_hint(&live)
        ),
    }
}

/// A trailing ` Live nodes: ...` fragment listing each live node's
/// `runtime_id` → `control_url`, for a resolver error message. Empty when there
/// are no live nodes.
fn render_node_hint(live: &[streamlib_api_server::node_registry::NodeRegistryEntry]) -> String {
    if live.is_empty() {
        return String::new();
    }
    let listed = live
        .iter()
        .map(|entry| format!("{} ({})", entry.runtime_id, entry.control_url))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" Live nodes: {listed}.")
}

/// POST one JSON-RPC request body to `{url}/mcp` and return the response body.
/// A 2xx yields the body (empty for a `202` notification); a non-2xx or
/// transport error is surfaced as an `Err`. `bearer_token`, when set, rides as
/// an `authorization: Bearer` header. This is the single request/response seam
/// the `mcp --attach` bridge and every control verb share.
pub fn post_mcp_request(
    url: &str,
    bearer_token: Option<&str>,
    request_body: &str,
) -> Result<String> {
    let endpoint = mcp_endpoint(url);
    let request = with_mcp_headers(ureq::post(&endpoint), bearer_token);
    match request.send_string(request_body) {
        Ok(response) => Ok(response.into_string()?),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            bail!("POST {endpoint} failed: HTTP {code}: {body}");
        }
        Err(error) => bail!("POST {endpoint} transport error: {error}"),
    }
}

/// Whether the control plane at `url` answers its `POST {url}/mcp` at all. Any
/// HTTP status — including an auth `4xx` — means the server is up (reachable);
/// only a transport error (connection refused, timeout) is dead. `connect_timeout`
/// and `response_timeout` bound a hung reused port so a probe never stalls a scan.
/// `bearer_token` rides as an `authorization: Bearer` header. This shares the
/// single `{url}/mcp` + bearer seam [`post_mcp_request`] owns, sending a `graph`
/// `tools/call` purely to elicit a response.
pub fn probe_reachable(
    url: &str,
    bearer_token: Option<&str>,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(connect_timeout)
        .timeout(response_timeout)
        .build();
    let endpoint = mcp_endpoint(url);
    let request = with_mcp_headers(agent.post(&endpoint), bearer_token);
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "graph", "arguments": {} },
    })
    .to_string();
    match request.send_string(&request_body) {
        Ok(_) => true,
        Err(ureq::Error::Status(_, _)) => true,
        Err(_) => false,
    }
}

/// The `{url}/mcp` control endpoint, with any trailing slash on `url` trimmed.
fn mcp_endpoint(url: &str) -> String {
    format!("{}/mcp", url.trim_end_matches('/'))
}

/// Apply the `content-type: application/json` and optional `authorization: Bearer`
/// headers every `{url}/mcp` POST carries.
fn with_mcp_headers(request: ureq::Request, bearer_token: Option<&str>) -> ureq::Request {
    let request = request.set("content-type", "application/json");
    match bearer_token {
        Some(bearer_token) => request.set("authorization", &format!("Bearer {bearer_token}")),
        None => request,
    }
}

/// Export the live runtime graph as JSON (`graph` tool).
pub fn graph(url: &str) -> Result<()> {
    call_tool_to_stdout(url, "graph", json!({}))
}

/// Attach a read-only tap to `channel` and collect a bounded sample (`tap`).
pub fn tap(url: &str, channel: &str, count: Option<usize>) -> Result<()> {
    let mut arguments = Map::new();
    arguments.insert("channel".into(), Value::String(channel.to_string()));
    insert_optional_count(&mut arguments, count);
    call_tool_to_stdout(url, "tap", Value::Object(arguments))
}

/// Collect a bounded sample of the runtime event stream (`logs`).
pub fn logs(url: &str, count: Option<usize>) -> Result<()> {
    let mut arguments = Map::new();
    insert_optional_count(&mut arguments, count);
    call_tool_to_stdout(url, "logs", Value::Object(arguments))
}

/// Ask a running node to shut down (`shutdown`). Returns as soon as the node
/// accepts the request — teardown is not awaited, so the node's control plane
/// may already be gone by the time the verb prints.
pub fn shutdown(url: &str, reason: Option<&str>) -> Result<()> {
    call_tool_to_stdout(url, "shutdown", shutdown_arguments(reason))
}

/// The `shutdown` tool's `arguments` object for an optional `--reason`. An
/// absent reason omits the key entirely rather than sending an explicit
/// `null`, which the tool's `inputSchema` (`reason` is a `string`) rejects.
fn shutdown_arguments(reason: Option<&str>) -> Value {
    let mut arguments = Map::new();
    if let Some(reason) = reason {
        arguments.insert("reason".into(), Value::String(reason.to_string()));
    }
    Value::Object(arguments)
}

/// Insert the optional `count` cap the `tap` and `logs` `inputSchema`s share.
fn insert_optional_count(arguments: &mut Map<String, Value>, count: Option<usize>) {
    if let Some(count) = count {
        arguments.insert("count".into(), json!(count));
    }
}

/// Drive one `tools/call` against `{url}/mcp` and print the result to stdout,
/// forwarding `STREAMLIB_MCP_TOKEN` as the bearer token when set.
fn call_tool_to_stdout(url: &str, tool_name: &str, arguments: Value) -> Result<()> {
    let bearer_token = std::env::var("STREAMLIB_MCP_TOKEN").ok();
    let stdout = std::io::stdout();
    call_tool(
        url,
        bearer_token.as_deref(),
        tool_name,
        arguments,
        &mut stdout.lock(),
    )
}

/// Marshal `arguments` into a `tools/call` for `tool_name`, POST it, and write
/// the tool result's text content to `writer`. Generic over the writer so a
/// test captures the output while the CLI wires process stdout. Covers the four
/// result channels described in the module docs.
fn call_tool(
    url: &str,
    bearer_token: Option<&str>,
    tool_name: &str,
    arguments: Value,
    writer: &mut impl Write,
) -> Result<()> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": arguments },
    });
    let body = post_mcp_request(url, bearer_token, &request.to_string())?;
    let response: Value = serde_json::from_str(&body)
        .with_context(|| format!("control plane returned a non-JSON response: {body}"))?;

    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown JSON-RPC error");
        bail!("{tool_name} failed: {message}");
    }

    let result = response
        .get("result")
        .with_context(|| format!("control plane response missing `result`: {body}"))?;
    let text = result
        .get("content")
        .and_then(|content| content.get(0))
        .and_then(|first| first.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("{tool_name} failed: {text}");
    }

    writeln!(writer, "{text}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Hermetic tests for the control verbs: a local TCP server stands in for a
    //! running node's `POST /mcp` (the same in-process mock pattern the
    //! `mcp --attach` bridge tests use), so `tools/call` marshaling, the four
    //! result channels, and arg parsing are exercised without a live runtime.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::*;

    struct RecordedRequest {
        authorization: Option<String>,
        body: String,
    }

    struct MockReply {
        status_line: &'static str,
        body: String,
    }

    type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

    /// Spin a local HTTP server answering `replies.len()` requests in order,
    /// recording each request's `authorization` header and body.
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
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
                stream.flush().ok();
            }
        });
        (url, recorded, handle)
    }

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

    /// A successful tool result carrying `value` as its pretty-JSON text block.
    fn tool_ok_reply(id: u64, value: Value) -> MockReply {
        let text = serde_json::to_string_pretty(&value).unwrap();
        MockReply {
            status_line: "200 OK",
            body: json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": [{ "type": "text", "text": text }], "isError": false },
            })
            .to_string(),
        }
    }

    #[test]
    fn graph_marshals_a_tools_call_and_prints_the_text_content() {
        let (url, recorded, server) =
            spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({ "processors": [] }))]);

        let mut output = Vec::new();
        call_tool(&url, None, "graph", json!({}), &mut output).expect("graph call");
        server.join().unwrap();

        let printed = String::from_utf8(output).unwrap();
        assert!(
            printed.contains("\"processors\""),
            "the tool result text content must be printed; got: {printed}"
        );

        let recorded = recorded.lock().unwrap();
        let request: Value = serde_json::from_str(&recorded[0].body).unwrap();
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "graph");
    }

    #[test]
    fn a_top_level_jsonrpc_error_exits_non_zero_with_the_message() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "200 OK",
            body: json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32601, "message": "method not found" },
            })
            .to_string(),
        }]);

        let mut output = Vec::new();
        let error =
            call_tool(&url, None, "graph", json!({}), &mut output).expect_err("must be an error");
        server.join().unwrap();

        assert!(
            error.to_string().contains("method not found"),
            "the JSON-RPC error.message must surface; got: {error}"
        );
        assert!(output.is_empty(), "no output line on an error");
    }

    #[test]
    fn a_tool_level_is_error_exits_non_zero_with_the_content_text() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "200 OK",
            body: json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{ "type": "text", "text": "connect failed: no such port" }],
                    "isError": true,
                },
            })
            .to_string(),
        }]);

        let mut output = Vec::new();
        let error = call_tool(&url, None, "connect", json!({}), &mut output)
            .expect_err("isError must surface as an error");
        server.join().unwrap();

        assert!(
            error.to_string().contains("no such port"),
            "the isError content text must surface; got: {error}"
        );
    }

    #[test]
    fn a_non_2xx_exits_non_zero_with_the_body() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "401 Unauthorized",
            body: "missing bearer".to_string(),
        }]);

        let mut output = Vec::new();
        let error = call_tool(&url, None, "graph", json!({}), &mut output)
            .expect_err("a non-2xx must surface as an error");
        server.join().unwrap();

        assert!(error.to_string().contains("401"), "got: {error}");
        assert!(error.to_string().contains("missing bearer"), "got: {error}");
    }

    #[test]
    fn probe_reachable_treats_any_http_status_including_401_as_alive() {
        let (url, _recorded, server) = spawn_mock_mcp_server(vec![MockReply {
            status_line: "401 Unauthorized",
            body: "missing bearer".to_string(),
        }]);
        let alive = probe_reachable(
            &url,
            None,
            Duration::from_millis(500),
            Duration::from_millis(1500),
        );
        server.join().unwrap();
        assert!(alive, "an HTTP response (even 401) means the server is up");
    }

    #[test]
    fn probe_reachable_reports_a_closed_port_as_dead() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!probe_reachable(
            &format!("http://127.0.0.1:{port}"),
            None,
            Duration::from_millis(500),
            Duration::from_millis(1500),
        ));
    }

    /// The `shutdown` verb must marshal into the `shutdown` tool with the
    /// caller's `--reason` in the arguments — a verb that named a different
    /// tool, or dropped the reason, would still print a result and exit 0.
    #[test]
    fn shutdown_marshals_the_reason_into_a_shutdown_tools_call() {
        let (url, recorded, server) = spawn_mock_mcp_server(vec![tool_ok_reply(
            1,
            json!({ "status": "RuntimeShutdownRequested", "reason": "cli asked" }),
        )]);

        let mut output = Vec::new();
        call_tool(
            &url,
            None,
            "shutdown",
            shutdown_arguments(Some("cli asked")),
            &mut output,
        )
        .expect("shutdown call");
        server.join().unwrap();

        let recorded = recorded.lock().unwrap();
        let request: Value = serde_json::from_str(&recorded[0].body).unwrap();
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "shutdown");
        assert_eq!(request["params"]["arguments"]["reason"], "cli asked");

        let printed = String::from_utf8(output).unwrap();
        assert!(
            printed.contains("RuntimeShutdownRequested"),
            "the accepted-request result must be printed; got: {printed}"
        );
    }

    /// Without `--reason`, the verb sends no `reason` key at all — the tool's
    /// `inputSchema` declares it optional and the server records "unspecified";
    /// sending an explicit `null` would fail the schema's type check.
    #[test]
    fn shutdown_without_a_reason_sends_no_reason_key() {
        let (url, recorded, server) = spawn_mock_mcp_server(vec![tool_ok_reply(
            1,
            json!({ "status": "RuntimeShutdownRequested", "reason": "" }),
        )]);

        let mut output = Vec::new();
        call_tool(
            &url,
            None,
            "shutdown",
            shutdown_arguments(None),
            &mut output,
        )
        .expect("shutdown call");
        server.join().unwrap();

        let recorded = recorded.lock().unwrap();
        let request: Value = serde_json::from_str(&recorded[0].body).unwrap();
        assert_eq!(
            request["params"]["arguments"],
            json!({}),
            "an absent --reason must marshal to empty arguments, not an explicit null"
        );
    }

    #[test]
    fn the_bearer_token_rides_as_an_authorization_header() {
        let (url, recorded, server) = spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({}))]);

        let mut output = Vec::new();
        call_tool(&url, Some("secret-token"), "graph", json!({}), &mut output).expect("graph call");
        server.join().unwrap();

        assert_eq!(
            recorded.lock().unwrap()[0].authorization.as_deref(),
            Some("Bearer secret-token")
        );
    }

    use serial_test::serial;
    use streamlib_api_server::node_registry::{self, NodeRegistryEntry};

    /// Point `XDG_RUNTIME_DIR` at a fresh tempdir for the closure so the node
    /// registry the resolver reads is hermetic; restore the prior value after.
    /// Guarded `#[serial]` at every call site — the env var is process-global.
    fn with_isolated_node_registry<F: FnOnce() -> R, R>(f: F) -> R {
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: callers are #[serial]; no concurrent env mutation.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        }
        let result = f();
        unsafe {
            match prev {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        result
    }

    fn write_node_entry(runtime_id: &str, control_url: &str) {
        node_registry::write_entry(&NodeRegistryEntry {
            schema_version: node_registry::NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id: runtime_id.to_string(),
            control_url: control_url.to_string(),
            pid: std::process::id(),
            hint: "test".to_string(),
        })
        .expect("write node entry");
    }

    #[test]
    fn resolve_control_url_prefers_an_explicit_url() {
        let resolved =
            resolve_control_url(Some("http://explicit:9000".to_string()), None).expect("resolve");
        assert_eq!(resolved, "http://explicit:9000");
    }

    #[test]
    #[serial]
    fn resolve_control_url_by_node_reads_the_registry_entry() {
        with_isolated_node_registry(|| {
            write_node_entry("Rpicked", "http://127.0.0.1:7777");
            let resolved =
                resolve_control_url(None, Some("Rpicked".to_string())).expect("resolve by node");
            assert_eq!(resolved, "http://127.0.0.1:7777");
        });
    }

    #[test]
    #[serial]
    fn resolve_control_url_by_unknown_node_errors() {
        with_isolated_node_registry(|| {
            let error = resolve_control_url(None, Some("Rghost".to_string()))
                .expect_err("unknown node must error");
            assert!(
                error.to_string().contains("Rghost"),
                "error must name the unknown runtime_id; got: {error}"
            );
        });
    }

    #[test]
    #[serial]
    fn resolve_control_url_defaults_to_the_sole_live_node() {
        // One reachable mock control plane answers the resolver's liveness probe.
        let (url, _recorded, server) =
            spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({ "processors": [] }))]);
        with_isolated_node_registry(|| {
            write_node_entry("Ronly", &url);
            let resolved = resolve_control_url(None, None).expect("sole live node resolves");
            assert_eq!(resolved, url);
        });
        server.join().unwrap();
    }

    #[test]
    #[serial]
    fn resolve_control_url_with_no_live_nodes_errors() {
        with_isolated_node_registry(|| {
            let error = resolve_control_url(None, None).expect_err("zero live nodes must error");
            assert!(error.to_string().contains("no live"), "got: {error}");
        });
    }

    #[test]
    #[serial]
    fn resolve_control_url_with_multiple_live_nodes_errors_and_lists_them() {
        let (url_a, _ra, server_a) = spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({}))]);
        let (url_b, _rb, server_b) = spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({}))]);
        with_isolated_node_registry(|| {
            write_node_entry("Rnode-a", &url_a);
            write_node_entry("Rnode-b", &url_b);
            let error =
                resolve_control_url(None, None).expect_err("more than one live node must error");
            let text = error.to_string();
            assert!(text.contains("Rnode-a"), "got: {text}");
            assert!(text.contains("Rnode-b"), "got: {text}");
        });
        server_a.join().unwrap();
        server_b.join().unwrap();
    }
}
