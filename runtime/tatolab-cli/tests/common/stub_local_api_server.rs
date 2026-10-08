// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A runtime's local API stood in by a stub on a fresh Unix socket: the official MCP SDK's server
//! answering scripted tool calls, beside the surface-image route answering scripted images and
//! the `/mcp/stdio` upgrade playing a scripted stream, each recording what it was sent. Shared by
//! the integration tests and, through `#[path]`, the unit tests; either mounts it beside
//! `tapped_channel_bag_fixtures`.

#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{Path as RoutePathSegment, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::serve::IncomingStream;
use hyper_util::rt::TokioIo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    MCP_STDIO_UPGRADE_PROTOCOL_TOKEN, MCP_STDIO_UPGRADE_REQUEST_TARGET,
    MCP_STREAMABLE_HTTP_ROUTE_PATH, SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE,
    SURFACE_PIXEL_HEIGHT_HEADER_NAME, SURFACE_PIXEL_WIDTH_HEADER_NAME,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::tapped_channel_bag_fixtures::empty_tap_result_text;

/// What the stub answers a tool call with when the script names no fixed answer.
pub const STUB_DEFAULT_TOOL_ANSWER_TEXT: &str = "{}";

/// The revision the stub serves, and the only one: a runtime's local API serves the latest alone.
const STUB_SERVED_MCP_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::LATEST];

/// A directory socket paths fit under: a socket path is capped near 104 bytes, and a
/// per-user temporary directory can eat most of that.
const SHORT_SOCKET_DIRECTORY_PARENT: &str = "/tmp";

/// The ordinal the next connection any stub in this process accepts is given.
static NEXT_STUB_ACCEPTED_CONNECTION_ORDINAL: AtomicU64 = AtomicU64::new(0);

/// Which accepted connection a request came over, unique across every stub in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StubAcceptedConnectionOrdinal(u64);

impl Connected<IncomingStream<'_, tokio::net::UnixListener>> for StubAcceptedConnectionOrdinal {
    fn connect_info(_accepted_stream: IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Self(NEXT_STUB_ACCEPTED_CONNECTION_ORDINAL.fetch_add(1, Ordering::Relaxed))
    }
}

/// How the stub answers one `tools/call`: the tool's text, and whether the tool ran and failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubToolAnswer {
    pub text: String,
    pub is_error: bool,
}

impl StubToolAnswer {
    /// A tool that ran and answered `text`.
    pub fn tool_result(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            is_error: false,
        }
    }

    /// A tool that ran and failed, saying `text`.
    pub fn tool_failure(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            is_error: true,
        }
    }
}

/// One `tools/call` the stub received, as the runtime would have.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedToolCall {
    pub tool_name: String,
    /// The call's arguments object; empty when it sent none.
    pub tool_arguments: serde_json::Value,
}

/// How the stub answers one `GET /api/surfaces/{surface_id}/image`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubSurfaceImageAnswer {
    pub http_status: u16,
    pub png_image_bytes: Vec<u8>,
    pub source_surface_pixel_width: Option<u32>,
    pub source_surface_pixel_height: Option<u32>,
    /// The `{"error": …}` a refusal carries.
    pub error_message: String,
}

impl StubSurfaceImageAnswer {
    /// A `200` carrying `png_image_bytes`, stating whichever extent headers are given.
    pub fn png_image(
        png_image_bytes: &[u8],
        source_surface_pixel_width: Option<u32>,
        source_surface_pixel_height: Option<u32>,
    ) -> Self {
        Self {
            http_status: 200,
            png_image_bytes: png_image_bytes.to_vec(),
            source_surface_pixel_width,
            source_surface_pixel_height,
            error_message: String::new(),
        }
    }

    /// A refusal with `http_status` and `{"error": error_message}` — a `410` is a recycled frame.
    pub fn refusal(http_status: u16, error_message: &str) -> Self {
        Self {
            http_status,
            png_image_bytes: Vec::new(),
            source_surface_pixel_width: None,
            source_surface_pixel_height: None,
            error_message: error_message.to_owned(),
        }
    }
}

/// `surface_image_answers` keyed by surface id, as [`StubLocalApiScript::surface_image_answers`]
/// holds them.
pub fn surface_image_answers_by_id<PublishedSurfaceId: Into<String>>(
    surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
) -> HashMap<String, StubSurfaceImageAnswer> {
    surface_image_answers
        .into_iter()
        .map(|(published_surface_id, answer)| (published_surface_id.into(), answer))
        .collect()
}

/// How the stub answers `GET /mcp/stdio`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StubMcpStdioUpgradeAnswer {
    /// Answer `101` and serve the stub's MCP server — the handler `/mcp` serves — over the
    /// upgraded stream until the client closes it, as a runtime does.
    #[default]
    ServeTheStubMcpServer,
    /// Answer `101`, write `written_once_upgraded`, echo every byte the client sends until it
    /// half-closes, then write `written_once_the_client_half_closed` and close.
    EchoUntilTheClientHalfCloses {
        written_once_upgraded: Vec<u8>,
        written_once_the_client_half_closed: Vec<u8>,
    },
    /// Answer `101` and close the upgraded stream at once, whatever the client still sends.
    CloseOnceUpgraded,
    /// Refuse the upgrade with `http_status` and an empty body.
    RefuseTheUpgrade { http_status: u16 },
    /// Read the upgrade request and never answer it.
    NeverAnswer,
}

/// One request head the stub received, as HTTP parsed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedHttpRequestHead {
    pub method: String,
    pub request_target: String,
    /// Every header line, its name lowercased by the parse, in no promised order.
    pub header_lines: Vec<(String, String)>,
}

/// What the stub answers. Tool calls drain `queued_tool_answers` in order, then
/// `fixed_tool_answer` answers forever, so a test names only the rounds it cares about.
#[derive(Debug, Clone, Default)]
pub struct StubLocalApiScript {
    /// Answers every call once the queue is drained; `{}` when unset.
    pub fixed_tool_answer: Option<StubToolAnswer>,
    pub queued_tool_answers: Vec<StubToolAnswer>,
    /// Refuse every call with an invalid-params MCP error carrying this message.
    pub refuse_every_tool_call_with: Option<String>,
    /// Answers by decoded surface id; an id not listed answers `404 {"error": "no such surface"}`.
    pub surface_image_answers: HashMap<String, StubSurfaceImageAnswer>,
    /// The tools `tools/list` names, each taking any object; none when empty.
    pub listed_tool_names: Vec<String>,
    /// How `/mcp/stdio` answers; serving the stub's MCP server when unset.
    pub mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer,
}

struct StubLocalApiState {
    fixed_tool_answer: StubToolAnswer,
    queued_tool_answers: Mutex<VecDeque<StubToolAnswer>>,
    refuse_every_tool_call_with: Option<String>,
    surface_image_answers: HashMap<String, StubSurfaceImageAnswer>,
    recorded_tool_calls: Mutex<Vec<RecordedToolCall>>,
    recorded_image_requests: Mutex<Vec<(String, StubAcceptedConnectionOrdinal)>>,
    listed_tool_names: Vec<String>,
    mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer,
    recorded_mcp_stdio_request_heads: Mutex<Vec<RecordedHttpRequestHead>>,
    recorded_mcp_stdio_client_bytes: Mutex<Vec<u8>>,
}

#[derive(Clone)]
struct StubLocalApiMcpServerHandler {
    stub_state: Arc<StubLocalApiState>,
}

impl ServerHandler for StubLocalApiMcpServerHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("stub-local-api", "0"))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(STUB_SERVED_MCP_PROTOCOL_VERSIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let any_object_input_schema = Arc::new(
            serde_json::json!({ "type": "object" })
                .as_object()
                .unwrap()
                .clone(),
        );
        Ok(ListToolsResult::with_all_items(
            self.stub_state
                .listed_tool_names
                .iter()
                .map(|listed_tool_name| {
                    Tool::new(
                        listed_tool_name.clone(),
                        "a scripted stub tool",
                        any_object_input_schema.clone(),
                    )
                })
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.stub_state
            .recorded_tool_calls
            .lock()
            .unwrap()
            .push(RecordedToolCall {
                tool_name: request.name.to_string(),
                tool_arguments: serde_json::Value::Object(request.arguments.unwrap_or_default()),
            });
        if let Some(refusal_message) = &self.stub_state.refuse_every_tool_call_with {
            return Err(ErrorData::invalid_params(refusal_message.clone(), None));
        }
        let answer = self
            .stub_state
            .queued_tool_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.stub_state.fixed_tool_answer.clone());
        let content = vec![ContentBlock::text(answer.text)];
        Ok(if answer.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
        .into())
    }
}

async fn answer_surface_image_request(
    State(stub_state): State<Arc<StubLocalApiState>>,
    ConnectInfo(accepted_connection_ordinal): ConnectInfo<StubAcceptedConnectionOrdinal>,
    RoutePathSegment(surface_id): RoutePathSegment<String>,
    request_uri: Uri,
) -> Response {
    stub_state
        .recorded_image_requests
        .lock()
        .unwrap()
        .push((request_uri.path().to_owned(), accepted_connection_ordinal));
    let answer = stub_state
        .surface_image_answers
        .get(&surface_id)
        .cloned()
        .unwrap_or_else(|| StubSurfaceImageAnswer::refusal(404, "no such surface"));
    let http_status = StatusCode::from_u16(answer.http_status).unwrap();
    if http_status != StatusCode::OK {
        return (
            http_status,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "error": answer.error_message }).to_string(),
        )
            .into_response();
    }
    let mut image_response = (
        http_status,
        [(header::CONTENT_TYPE, "image/png")],
        answer.png_image_bytes,
    )
        .into_response();
    for (extent_header, extent) in [
        (
            SURFACE_PIXEL_WIDTH_HEADER_NAME,
            answer.source_surface_pixel_width,
        ),
        (
            SURFACE_PIXEL_HEIGHT_HEADER_NAME,
            answer.source_surface_pixel_height,
        ),
    ] {
        if let Some(extent) = extent {
            image_response
                .headers_mut()
                .insert(extent_header, extent.to_string().parse().unwrap());
        }
    }
    image_response
}

/// `/mcp/stdio`: record the head, then refuse or answer `101` and play the scripted stream.
async fn answer_mcp_stdio_upgrade_request(
    State(stub_state): State<Arc<StubLocalApiState>>,
    mut upgrade_request: axum::extract::Request,
) -> Response {
    stub_state
        .recorded_mcp_stdio_request_heads
        .lock()
        .unwrap()
        .push(RecordedHttpRequestHead {
            method: upgrade_request.method().to_string(),
            request_target: upgrade_request.uri().to_string(),
            header_lines: upgrade_request
                .headers()
                .iter()
                .map(|(header_name, header_value)| {
                    (
                        header_name.as_str().to_owned(),
                        header_value.to_str().unwrap().to_owned(),
                    )
                })
                .collect(),
        });
    match stub_state.mcp_stdio_upgrade_answer {
        StubMcpStdioUpgradeAnswer::RefuseTheUpgrade { http_status } => {
            return StatusCode::from_u16(http_status).unwrap().into_response();
        }
        StubMcpStdioUpgradeAnswer::NeverAnswer => {
            return std::future::pending::<Response>().await;
        }
        StubMcpStdioUpgradeAnswer::ServeTheStubMcpServer
        | StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses { .. }
        | StubMcpStdioUpgradeAnswer::CloseOnceUpgraded => {}
    }
    let pending_upgrade = hyper::upgrade::on(&mut upgrade_request);
    tokio::spawn(async move {
        if let Ok(upgraded_connection) = pending_upgrade.await {
            play_the_upgraded_mcp_stdio_stream(stub_state, TokioIo::new(upgraded_connection)).await;
        }
    });
    (
        StatusCode::SWITCHING_PROTOCOLS,
        [
            (header::CONNECTION, "upgrade"),
            (header::UPGRADE, MCP_STDIO_UPGRADE_PROTOCOL_TOKEN),
        ],
    )
        .into_response()
}

async fn play_the_upgraded_mcp_stdio_stream(
    stub_state: Arc<StubLocalApiState>,
    mut upgraded_mcp_stdio_stream: TokioIo<hyper::upgrade::Upgraded>,
) {
    match stub_state.mcp_stdio_upgrade_answer.clone() {
        StubMcpStdioUpgradeAnswer::ServeTheStubMcpServer => {
            let mcp_server_handler = StubLocalApiMcpServerHandler { stub_state };
            if let Ok(running_mcp_server) =
                mcp_server_handler.serve(upgraded_mcp_stdio_stream).await
            {
                let _served_until_the_client_closed = running_mcp_server.waiting().await;
            }
        }
        StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
            written_once_upgraded,
            written_once_the_client_half_closed,
        } => {
            if upgraded_mcp_stdio_stream
                .write_all(&written_once_upgraded)
                .await
                .is_err()
            {
                return;
            }
            let mut client_chunk = [0_u8; 4096];
            loop {
                let read_byte_count = match upgraded_mcp_stdio_stream.read(&mut client_chunk).await
                {
                    Ok(0) | Err(_) => break,
                    Ok(read_byte_count) => read_byte_count,
                };
                stub_state
                    .recorded_mcp_stdio_client_bytes
                    .lock()
                    .unwrap()
                    .extend_from_slice(&client_chunk[..read_byte_count]);
                if upgraded_mcp_stdio_stream
                    .write_all(&client_chunk[..read_byte_count])
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _written_or_the_client_gone = upgraded_mcp_stdio_stream
                .write_all(&written_once_the_client_half_closed)
                .await;
            let _closed_or_already_gone = upgraded_mcp_stdio_stream.shutdown().await;
        }
        StubMcpStdioUpgradeAnswer::CloseOnceUpgraded
        | StubMcpStdioUpgradeAnswer::RefuseTheUpgrade { .. }
        | StubMcpStdioUpgradeAnswer::NeverAnswer => {}
    }
}

/// The stub's router: `/mcp` as a runtime's local API configures it — stateless, JSON answers, no
/// allowed-hosts check since the socket's file mode is the gate — the surface-image route, and the
/// `/mcp/stdio` upgrade. Axum routes on the request target's path, so `rmcp`'s absolute-form `POST
/// http://localhost/mcp` reaches `/mcp` as RFC 9112 §3.2.2 requires.
fn stub_local_api_router(stub_state: Arc<StubLocalApiState>) -> axum::Router {
    let mcp_server_handler = StubLocalApiMcpServerHandler {
        stub_state: stub_state.clone(),
    };
    let local_api_mcp_service = StreamableHttpService::new(
        move || Ok(mcp_server_handler.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_stateless_protocol_metadata_required(true)
            .with_json_response(true)
            .disable_allowed_hosts(),
    );
    axum::Router::new()
        .route_service(MCP_STREAMABLE_HTTP_ROUTE_PATH, local_api_mcp_service)
        .route(
            SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE,
            axum::routing::get(answer_surface_image_request),
        )
        .route(
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
            axum::routing::get(answer_mcp_stdio_upgrade_request),
        )
        .with_state(stub_state)
}

/// A stub local API being served; dropping it stops serving and removes its socket.
pub struct StubLocalApiServer {
    /// The socket the stub answers on.
    pub local_api_socket_path: PathBuf,
    stub_state: Arc<StubLocalApiState>,
    stop_serving: Option<tokio::sync::oneshot::Sender<()>>,
    serving_thread: Option<std::thread::JoinHandle<()>>,
    local_api_socket_directory: tempfile::TempDir,
}

impl StubLocalApiServer {
    /// Serve `stub_local_api_script` on a fresh socket; it accepts connections once this returns.
    pub fn serve(stub_local_api_script: StubLocalApiScript) -> Self {
        let local_api_socket_directory = tempfile::Builder::new()
            .prefix("tl-stub-")
            .tempdir_in(SHORT_SOCKET_DIRECTORY_PARENT)
            .unwrap();
        let local_api_socket_path = local_api_socket_directory.path().join("local-api.sock");
        let bound_listener =
            std::os::unix::net::UnixListener::bind(&local_api_socket_path).unwrap();
        bound_listener.set_nonblocking(true).unwrap();

        let stub_state = Arc::new(StubLocalApiState {
            fixed_tool_answer: stub_local_api_script
                .fixed_tool_answer
                .unwrap_or_else(|| StubToolAnswer::tool_result(STUB_DEFAULT_TOOL_ANSWER_TEXT)),
            queued_tool_answers: Mutex::new(stub_local_api_script.queued_tool_answers.into()),
            refuse_every_tool_call_with: stub_local_api_script.refuse_every_tool_call_with,
            surface_image_answers: stub_local_api_script.surface_image_answers,
            recorded_tool_calls: Mutex::new(Vec::new()),
            recorded_image_requests: Mutex::new(Vec::new()),
            listed_tool_names: stub_local_api_script.listed_tool_names,
            mcp_stdio_upgrade_answer: stub_local_api_script.mcp_stdio_upgrade_answer,
            recorded_mcp_stdio_request_heads: Mutex::new(Vec::new()),
            recorded_mcp_stdio_client_bytes: Mutex::new(Vec::new()),
        });
        let (stop_serving, serving_stopped) = tokio::sync::oneshot::channel::<()>();
        let served_stub_state = stub_state.clone();
        let serving_thread = std::thread::spawn(move || {
            let serving_tokio_runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            serving_tokio_runtime.block_on(async move {
                let local_api_listener =
                    tokio::net::UnixListener::from_std(bound_listener).unwrap();
                tokio::spawn(
                    axum::serve(
                        local_api_listener,
                        stub_local_api_router(served_stub_state)
                            .into_make_service_with_connect_info::<StubAcceptedConnectionOrdinal>(),
                    )
                    .into_future(),
                );
                // Dropping the runtime on return ends every connection still open.
                let _stopped_or_abandoned = serving_stopped.await;
            });
        });
        Self {
            local_api_socket_path,
            stub_state,
            stop_serving: Some(stop_serving),
            serving_thread: Some(serving_thread),
            local_api_socket_directory,
        }
    }

    /// Serve a stub that answers every tool call with `stub_tool_answer`.
    pub fn serve_answering_every_tool_call_with(stub_tool_answer: StubToolAnswer) -> Self {
        Self::serve(StubLocalApiScript {
            fixed_tool_answer: Some(stub_tool_answer),
            ..StubLocalApiScript::default()
        })
    }

    /// Serve a stub that answers every tool call `{}`.
    pub fn serve_default() -> Self {
        Self::serve(StubLocalApiScript::default())
    }

    /// Serve a stub whose surface-image route answers `surface_image_answers` by surface id.
    pub fn serve_answering_surface_images<PublishedSurfaceId: Into<String>>(
        surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
    ) -> Self {
        Self::serve(StubLocalApiScript {
            surface_image_answers: surface_image_answers_by_id(surface_image_answers),
            ..StubLocalApiScript::default()
        })
    }

    /// Serve a stub whose `tap` answers `queued_tap_results` in order and then an empty round
    /// forever, and whose surface-image route answers `surface_image_answers` by surface id.
    pub fn serve_tapping<PublishedSurfaceId: Into<String>>(
        queued_tap_results: &[String],
        surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
    ) -> Self {
        Self::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result(&empty_tap_result_text())),
            queued_tool_answers: queued_tap_results
                .iter()
                .map(|tap_result| StubToolAnswer::tool_result(tap_result))
                .collect(),
            surface_image_answers: surface_image_answers_by_id(surface_image_answers),
            ..StubLocalApiScript::default()
        })
    }

    /// Serve a stub whose `/mcp/stdio` answers as `mcp_stdio_upgrade_answer` says.
    pub fn serve_answering_the_mcp_stdio_upgrade_with(
        mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer,
    ) -> Self {
        Self::serve(StubLocalApiScript {
            mcp_stdio_upgrade_answer,
            ..StubLocalApiScript::default()
        })
    }

    /// Every tool call received so far, in arrival order.
    pub fn recorded_tool_calls(&self) -> Vec<RecordedToolCall> {
        self.stub_state.recorded_tool_calls.lock().unwrap().clone()
    }

    /// The path of every surface-image request received so far, percent-encoded as sent.
    pub fn recorded_image_request_paths(&self) -> Vec<String> {
        self.stub_state
            .recorded_image_requests
            .lock()
            .unwrap()
            .iter()
            .map(|(image_request_path, _)| image_request_path.clone())
            .collect()
    }

    /// How many distinct connections the surface-image requests so far came over.
    pub fn image_request_connection_count(&self) -> usize {
        self.stub_state
            .recorded_image_requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, accepted_connection_ordinal)| *accepted_connection_ordinal)
            .collect::<HashSet<StubAcceptedConnectionOrdinal>>()
            .len()
    }

    /// The head of every `/mcp/stdio` request received so far, in arrival order.
    pub fn recorded_mcp_stdio_request_heads(&self) -> Vec<RecordedHttpRequestHead> {
        self.stub_state
            .recorded_mcp_stdio_request_heads
            .lock()
            .unwrap()
            .clone()
    }

    /// Every byte a client sent over an echoing `/mcp/stdio` stream so far, in arrival order.
    pub fn recorded_mcp_stdio_client_bytes(&self) -> Vec<u8> {
        self.stub_state
            .recorded_mcp_stdio_client_bytes
            .lock()
            .unwrap()
            .clone()
    }
}

impl Drop for StubLocalApiServer {
    fn drop(&mut self) {
        if let Some(stop_serving) = self.stop_serving.take() {
            let _serving_already_ended = stop_serving.send(());
        }
        if let Some(serving_thread) = self.serving_thread.take() {
            let _serving_thread_outcome = serving_thread.join();
        }
    }
}
