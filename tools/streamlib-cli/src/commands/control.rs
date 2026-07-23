// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib graph | submit | replace | remove | connect | tap | logs` — thin
//! JSON-RPC control clients over a running node's `POST {url}/mcp`.
//!
//! Each verb marshals its args into a `tools/call` for one of the 7 api-server
//! MCP tools ([`streamlib_api_server`]'s `tool_definitions`) and POSTs it over
//! the same `ureq` seam the `mcp --attach` bridge uses ([`post_mcp_request`],
//! shared with [`super::mcp`]). There is no local runtime and no fourth
//! dispatch: the tool set is exactly those 7, and the arg shapes mirror each
//! tool's `inputSchema` 1:1.
//!
//! The optional `STREAMLIB_MCP_TOKEN` rides as an `authorization: Bearer`
//! header, matching the `--attach` bridge. Result handling covers four
//! channels: a non-2xx HTTP status, a top-level JSON-RPC `error` (returned
//! inside an HTTP 200), a tool-level `result.isError`, and success — the first
//! three exit non-zero with the error text, the last prints the tool result's
//! already-pretty text content.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

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
    let endpoint = format!("{}/mcp", url.trim_end_matches('/'));
    let mut request = ureq::post(&endpoint).set("content-type", "application/json");
    if let Some(bearer_token) = bearer_token {
        request = request.set("authorization", &format!("Bearer {bearer_token}"));
    }
    match request.send_string(request_body) {
        Ok(response) => Ok(response.into_string()?),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            bail!("POST {endpoint} failed: HTTP {code}: {body}");
        }
        Err(error) => bail!("POST {endpoint} transport error: {error}"),
    }
}

/// Export the live runtime graph as JSON (`graph` tool).
pub fn graph(url: &str) -> Result<()> {
    call_tool_to_stdout(url, "graph", json!({}))
}

/// Arguments for the `submit` verb, mirroring the `submit_processor`
/// `inputSchema`. `source` is a `--source` value (`@file`, a plain path, `-`,
/// or absent → stdin); `config` is a JSON string (absent → `{}`); each
/// `connect` entry is a `local_port:role:peer_processor:peer_port` spec.
pub struct SubmitArgs {
    pub url: String,
    pub language: String,
    pub source: Option<String>,
    pub requested_name: Option<String>,
    pub processor_type_name: Option<String>,
    pub config: Option<String>,
    pub connect: Vec<String>,
}

/// Register a processor from source and optionally wire it (`submit_processor`).
pub fn submit(args: SubmitArgs) -> Result<()> {
    let source = read_source(args.source.as_deref())?;
    let config = parse_config(args.config.as_deref())?;
    let connect = args
        .connect
        .iter()
        .map(|spec| parse_connect_spec(spec))
        .collect::<Result<Vec<_>>>()?;

    let mut arguments = json!({
        "language": args.language,
        "source": source,
        "config": config,
    });
    let map = arguments
        .as_object_mut()
        .expect("json! object literal is always an object");
    if let Some(requested_name) = args.requested_name {
        map.insert("requested_name".into(), Value::String(requested_name));
    }
    if let Some(processor_type_name) = args.processor_type_name {
        map.insert(
            "processor_type_name".into(),
            Value::String(processor_type_name),
        );
    }
    if !connect.is_empty() {
        map.insert("connect".into(), Value::Array(connect));
    }

    call_tool_to_stdout(&args.url, "submit_processor", arguments)
}

/// Arguments for the `replace` verb, mirroring the `replace_processor`
/// `inputSchema`.
pub struct ReplaceArgs {
    pub url: String,
    pub target_session_module: String,
    pub language: String,
    pub source: Option<String>,
    pub requested_name: Option<String>,
    pub processor_type_name: Option<String>,
}

/// Swap a live `@session/<name>` source registration for a replacement
/// (`replace_processor`). Type-level: already-running instances are not swapped
/// in place.
pub fn replace(args: ReplaceArgs) -> Result<()> {
    let source = read_source(args.source.as_deref())?;
    let mut arguments = json!({
        "target_session_module": args.target_session_module,
        "language": args.language,
        "source": source,
    });
    let map = arguments
        .as_object_mut()
        .expect("json! object literal is always an object");
    if let Some(requested_name) = args.requested_name {
        map.insert("requested_name".into(), Value::String(requested_name));
    }
    if let Some(processor_type_name) = args.processor_type_name {
        map.insert(
            "processor_type_name".into(),
            Value::String(processor_type_name),
        );
    }
    call_tool_to_stdout(&args.url, "replace_processor", arguments)
}

/// Remove a processor instance from the graph by id (`remove_processor`).
pub fn remove(url: &str, processor_id: &str) -> Result<()> {
    call_tool_to_stdout(
        url,
        "remove_processor",
        json!({ "processor_id": processor_id }),
    )
}

/// Connect an output port to an input port between two existing processors
/// (`connect`).
pub fn connect(
    url: &str,
    from_processor: &str,
    from_port: &str,
    to_processor: &str,
    to_port: &str,
) -> Result<()> {
    call_tool_to_stdout(
        url,
        "connect",
        json!({
            "from_processor": from_processor,
            "from_port": from_port,
            "to_processor": to_processor,
            "to_port": to_port,
        }),
    )
}

/// Attach a read-only tap to `channel` and collect a bounded sample (`tap`).
pub fn tap(url: &str, channel: &str, count: Option<usize>) -> Result<()> {
    let mut arguments = json!({ "channel": channel });
    if let Some(count) = count {
        arguments
            .as_object_mut()
            .expect("json! object literal is always an object")
            .insert("count".into(), json!(count));
    }
    call_tool_to_stdout(url, "tap", arguments)
}

/// Collect a bounded sample of the runtime event stream (`logs`).
pub fn logs(url: &str, count: Option<usize>) -> Result<()> {
    let mut arguments = json!({});
    if let Some(count) = count {
        arguments
            .as_object_mut()
            .expect("json! object literal is always an object")
            .insert("count".into(), json!(count));
    }
    call_tool_to_stdout(url, "logs", arguments)
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

/// Resolve a `--source` value to processor source text: `@<path>` or a plain
/// `<path>` reads the file; `-` or an absent value reads stdin.
fn read_source(source_arg: Option<&str>) -> Result<String> {
    let path = match source_arg {
        None => return read_stdin(),
        Some(value) => value.strip_prefix('@').unwrap_or(value),
    };
    if path == "-" {
        return read_stdin();
    }
    std::fs::read_to_string(path).with_context(|| format!("reading --source file `{path}`"))
}

/// Read all of stdin as UTF-8 source text.
fn read_stdin() -> Result<String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .context("reading --source from stdin")?;
    Ok(buffer)
}

/// Parse a `--config` value as JSON; an absent value defaults to `{}`.
fn parse_config(config_arg: Option<&str>) -> Result<Value> {
    match config_arg {
        None => Ok(json!({})),
        Some(text) => serde_json::from_str(text)
            .with_context(|| format!("--config is not valid JSON: {text}")),
    }
}

/// Parse one `--connect` spec (`local_port:role:peer_processor:peer_port`) into
/// the `connect[]` item shape the `submit_processor` `inputSchema` requires.
fn parse_connect_spec(spec: &str) -> Result<Value> {
    let fields: Vec<&str> = spec.split(':').collect();
    if fields.len() != 4 {
        bail!(
            "--connect must be `local_port:role:peer_processor:peer_port`; got `{spec}` \
             ({} field(s))",
            fields.len()
        );
    }
    let role = fields[1];
    if role != "output" && role != "input" {
        bail!("--connect role must be `output` or `input`; got `{role}` in `{spec}`");
    }
    Ok(json!({
        "local_port": fields[0],
        "role": role,
        "peer_processor": fields[2],
        "peer_port": fields[3],
    }))
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
    fn submit_arguments_carry_config_and_connect_wirings() {
        let (url, recorded, server) =
            spawn_mock_mcp_server(vec![tool_ok_reply(1, json!({ "processors": [] }))]);

        let arguments = json!({
            "language": "python",
            "source": "class Widget: pass\n",
            "config": { "gain": 2 },
            "connect": [parse_connect_spec("out:output:sink:in").unwrap()],
        });
        let mut output = Vec::new();
        call_tool(&url, None, "submit_processor", arguments, &mut output).expect("submit call");
        server.join().unwrap();

        let recorded = recorded.lock().unwrap();
        let request: Value = serde_json::from_str(&recorded[0].body).unwrap();
        let args = &request["params"]["arguments"];
        assert_eq!(args["language"], "python");
        assert_eq!(args["config"]["gain"], 2);
        assert_eq!(args["connect"][0]["local_port"], "out");
        assert_eq!(args["connect"][0]["role"], "output");
        assert_eq!(args["connect"][0]["peer_processor"], "sink");
        assert_eq!(args["connect"][0]["peer_port"], "in");
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

    #[test]
    fn parse_connect_spec_rejects_a_bad_arity_and_role() {
        assert!(
            parse_connect_spec("a:output:b").is_err(),
            "3 fields must fail"
        );
        assert!(
            parse_connect_spec("a:sideways:b:c").is_err(),
            "an unknown role must fail"
        );
        let ok = parse_connect_spec("a:input:b:c").expect("valid spec");
        assert_eq!(ok["role"], "input");
    }

    #[test]
    fn parse_config_defaults_to_empty_object() {
        assert_eq!(parse_config(None).unwrap(), json!({}));
        assert_eq!(parse_config(Some(r#"{"x":1}"#)).unwrap(), json!({ "x": 1 }));
        assert!(parse_config(Some("not json")).is_err());
    }

    #[test]
    fn read_source_reads_an_at_file_and_a_plain_path() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(file, "source text\n").unwrap();
        let path = file.path().to_str().unwrap();

        assert_eq!(
            read_source(Some(&format!("@{path}"))).unwrap(),
            "source text\n"
        );
        assert_eq!(read_source(Some(path)).unwrap(), "source text\n");
        assert!(
            read_source(Some("@/no/such/file/here")).is_err(),
            "a missing file must error"
        );
    }
}
