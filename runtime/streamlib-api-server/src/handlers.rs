// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! HTTP + WebSocket handlers, wired into the router by [`build_router`].

use axum::{
    Json, Router,
    extract::Query,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::json_schema::{
    GraphResponse, MachineWideGraphResponse, MachineWideRegistryResponse,
    NodeTypesDescribedInOneStreamOutput, ProcessorDescriptorOutput, RegistryResponse,
};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations};
use streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine;
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    MCP_STDIO_UPGRADE_REQUEST_TARGET, MCP_STREAMABLE_HTTP_ROUTE_PATH,
};

use crate::state::{
    ApiDoc, AppState, ErrorResponse, OptionalStreamSelectionQuery, RelayAdapterCatalogResponse,
};

// ============================================================================
// Router Construction
// ============================================================================

/// The REST routes the control plane serves, each with its OpenAPI registration.
fn control_plane_rest_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(get_graph))
        .routes(routes!(get_registry))
        .routes(routes!(get_relay_adapters))
}

/// The OpenAPI document for the REST surface, built from the same route
/// registrations `build_router` installs.
///
/// The codegen binary reads the spec through here rather than declaring its own
/// paths: a second inventory drifts silently, and its drift ships in the
/// generated client rather than failing a build.
pub fn control_plane_openapi_spec() -> utoipa::openapi::OpenApi {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(control_plane_rest_routes())
        .split_for_parts()
        .1
}

/// Build the full router with shared state and trace layer attached.
///
/// The REST surface is observation-shaped: a stream's graph is defined by its
/// code, so no route creates, replaces, connects, or removes a processor. No
/// route asks for a credential: whoever can open the local API socket may call
/// every one. `local_api_stopping_token` is
/// cancelled when the local API stops serving, ending every
/// `subscriptions/listen` `/mcp` holds open and every stream `/mcp/stdio`
/// upgraded.
pub(crate) fn build_router(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
    local_api_stopping_token: CancellationToken,
) -> Router {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(control_plane_rest_routes())
        .split_for_parts();

    let local_api_mcp_server_handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::clone(&operations_on_the_loaded_streams),
        Arc::clone(&relay_instances),
        local_api_stopping_token.clone(),
    );
    let local_api_mcp_service =
        crate::mcp::local_api_mcp_streamable_http_service(local_api_mcp_server_handler.clone());
    let state = AppState {
        operations_on_the_loaded_streams,
        relay_instances,
        openapi,
    };

    // Method, path, status and latency for every request, at DEBUG so a client
    // polling the node stays out of the app's own log at the default `info`
    // filter; `RUST_LOG=tower_http=debug` is how you ask for it. The on-failure
    // hook keeps its ERROR default — a request that fails is news either way.
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::DEBUG))
        .on_request(DefaultOnRequest::new().level(Level::DEBUG))
        .on_response(DefaultOnResponse::new().level(Level::DEBUG));

    let router = router
        .route("/ws/events", get(websocket_handler))
        .route("/api/openapi.json", get(get_openapi_spec))
        .route_service(MCP_STREAMABLE_HTTP_ROUTE_PATH, local_api_mcp_service)
        .route(
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
            crate::mcp_stdio_upgrade::local_api_mcp_stdio_upgrade_route(
                local_api_mcp_server_handler,
                local_api_stopping_token,
            ),
        );

    router.layer(trace_layer).with_state(state)
}

// ============================================================================
// API Handlers
// ============================================================================

#[utoipa::path(
    get,
    path = "/health",
    tag = "graph",
    responses(
        (status = 200, description = "Server is healthy", body = String)
    )
)]
pub(crate) async fn health() -> &'static str {
    "ok"
}

/// What a route answers when the stream it was asked about cannot be named:
/// `404` for one not loaded — the refusal names the loaded streams — `400` for
/// a name that casts to nothing, and `500` for anything else.
fn stream_resolution_refusal_response(refusal: &Error) -> Response {
    let status = match refusal {
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::ExposedNameCastsToNothing(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_response(status, refusal)
}

/// The operations on the loaded stream `stream` names, or the response
/// refusing the call.
fn stream_operations_or_refusal(
    state: &AppState,
    stream: &str,
) -> std::result::Result<Arc<dyn RuntimeOperations>, Box<Response>> {
    state
        .operations_on_the_loaded_streams
        .runtime_operations_of_the_stream_a_call_names(stream)
        .map_err(|refusal| Box::new(stream_resolution_refusal_response(&refusal)))
}

/// What `GET /api/graph` answers: one stream's graph, or every loaded
/// stream's under the machine's name.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
#[expect(
    dead_code,
    reason = "documents the route's two answers; each is served as the engine renders it, with \
              every live key the typed form would drop"
)]
pub(crate) enum GraphOfOneStreamOrOfEveryStream {
    /// The graph of the stream `stream` named.
    OneStream(GraphResponse),
    /// Every loaded stream's graph, when no `stream` was named.
    EveryStream(MachineWideGraphResponse),
}

/// What `GET /api/registry` answers: one stream's node catalog, or the native
/// types and each loaded stream's own.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub(crate) enum NodeTypesOfOneStreamOrOfEveryStream {
    /// The node types the stream `stream` named can add.
    OneStream(RegistryResponse),
    /// The native types and each loaded stream's Python types, when no
    /// `stream` was named.
    EveryStream(MachineWideRegistryResponse),
}

/// Every loaded stream's graph under the machine's name: what `/api/graph`,
/// the `graph` tool and the `streamlib://graph` resource answer when no
/// stream is named. A stream unloaded while this renders is left out.
pub(crate) async fn machine_wide_graph_json(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Result<Value> {
    let mut stream_graphs = Vec::new();
    for stream_id in operations_on_the_loaded_streams.ids_of_the_loaded_streams() {
        let stream_operations = match operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(&stream_id)
        {
            Ok(stream_operations) => stream_operations,
            Err(Error::NotFound(_)) => continue,
            Err(other) => return Err(other),
        };
        stream_graphs.push(stream_operations.to_json_async().await?);
    }
    Ok(json!({
        "machine": operations_on_the_loaded_streams.this_machines_name().name(),
        "streams": stream_graphs,
    }))
}

/// The native node types every stream can add, then each loaded stream's
/// types described in its own interpreter: what `/api/registry` and the
/// `streamlib://node-catalog` resource answer when no stream is named. A
/// stream unloaded while this renders is left out.
pub(crate) fn machine_wide_registry_response(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> MachineWideRegistryResponse {
    MachineWideRegistryResponse {
        nodes: node_catalog_response(&PROCESSOR_REGISTRY.list_registered()).nodes,
        streams: operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .into_iter()
            .filter_map(|stream_id| {
                let described = operations_on_the_loaded_streams
                    .node_types_described_in_the_interpreter_of_the_stream_a_call_names(&stream_id)
                    .ok()?;
                Some(NodeTypesDescribedInOneStreamOutput {
                    stream: stream_id,
                    nodes: node_catalog_response(&described).nodes,
                })
            })
            .collect(),
    }
}

/// `status` with `error` as an [`ErrorResponse`] body.
pub(crate) fn error_response(status: StatusCode, error: &impl std::fmt::Display) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/graph",
    tag = "graph",
    params(
        ("stream" = Option<String>, Query, description = "The loaded stream whose graph to export, by its stream id; absent exports every loaded stream's under the machine's name")
    ),
    responses(
        (status = 200, description = "With `stream`, that stream's current graph; without, `{machine, streams}` holding every loaded stream's", body = GraphOfOneStreamOrOfEveryStream),
        (status = 404, description = "The named stream is not loaded; the error names the loaded streams", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub(crate) async fn get_graph(
    State(state): State<AppState>,
    Query(OptionalStreamSelectionQuery { stream }): Query<OptionalStreamSelectionQuery>,
) -> Response {
    let Some(stream) = stream else {
        return match machine_wide_graph_json(&state.operations_on_the_loaded_streams).await {
            Ok(graphs) => Json(graphs).into_response(),
            Err(export_failure) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &export_failure)
            }
        };
    };
    let stream_operations = match stream_operations_or_refusal(&state, &stream) {
        Ok(stream_operations) => stream_operations,
        Err(refusal) => return *refusal,
    };
    match stream_operations.to_json_async().await {
        Ok(graph) => Json(graph).into_response(),
        Err(export_failure) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &export_failure),
    }
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    params(
        ("stream" = Option<String>, Query, description = "The loaded stream whose node catalog to list, by its stream id; absent lists the native types and each loaded stream's own")
    ),
    responses(
        (status = 200, description = "With `stream`, the node types that stream can add: the natively compiled ones, then the ones described in its own interpreter. Without, `{nodes, streams}`: the native types, then each loaded stream's described types under that stream", body = NodeTypesOfOneStreamOrOfEveryStream),
        (status = 404, description = "The named stream is not loaded; the error names the loaded streams", body = ErrorResponse)
    )
)]
pub(crate) async fn get_registry(
    State(state): State<AppState>,
    Query(OptionalStreamSelectionQuery { stream }): Query<OptionalStreamSelectionQuery>,
) -> Response {
    let Some(stream) = stream else {
        return Json(NodeTypesOfOneStreamOrOfEveryStream::EveryStream(
            machine_wide_registry_response(&state.operations_on_the_loaded_streams),
        ))
        .into_response();
    };
    match state
        .operations_on_the_loaded_streams
        .node_catalog_of_the_stream_a_call_names(&stream)
    {
        Ok(node_catalog) => Json(NodeTypesOfOneStreamOrOfEveryStream::OneStream(
            node_catalog_response(&node_catalog),
        ))
        .into_response(),
        Err(refusal) => stream_resolution_refusal_response(&refusal),
    }
}

#[utoipa::path(
    get,
    path = "/api/relay-adapters",
    tag = "relays",
    responses(
        (status = 200, description = "Every relay adapter compiled into the runtime: its kind, as `add_relay` takes it, the name people read it by, the provider's logo as an inline `data:` URI, what it joins, and its settings, each with its name, its field type, whether it is secret, whether it is required, its doc and its default", body = RelayAdapterCatalogResponse)
    )
)]
pub(crate) async fn get_relay_adapters(State(state): State<AppState>) -> Response {
    Json(RelayAdapterCatalogResponse::of(
        state.relay_instances.as_ref(),
    ))
    .into_response()
}

/// One stream's node catalog, rendered as `/api/registry` and the MCP node
/// catalog resource both serve it.
pub(crate) fn node_catalog_response(node_catalog: &[ProcessorDescriptor]) -> RegistryResponse {
    RegistryResponse {
        nodes: node_catalog
            .iter()
            .map(ProcessorDescriptorOutput::from)
            .collect(),
    }
}

pub(crate) async fn get_openapi_spec(
    State(state): State<AppState>,
) -> Json<utoipa::openapi::OpenApi> {
    Json(state.openapi)
}

// ============================================================================
// WebSocket Event Streaming
// ============================================================================

pub(crate) async fn websocket_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_websocket)
}

async fn handle_websocket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    // Channel to bridge sync EventListener -> async WebSocket
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(WebSocketEventForwarder { tx }));
    // Closing beats serving a client that would receive nothing forever.
    if let Err(subscribe_error) = PUBSUB.subscribe(topics::ALL, Arc::clone(&listener)) {
        tracing::warn!("WebSocket client not subscribed, closing: {subscribe_error}");
        return;
    }

    tracing::info!("WebSocket client connected, subscribed to all events");

    // Task: forward channel events to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to serialize event: {}", e);
                }
            }
        }
    });

    // Receive loop (keep-alive, handle close)
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Close(_)) => {
                tracing::info!("WebSocket client closed connection");
                break;
            }
            Err(e) => {
                tracing::warn!("WebSocket error: {}", e);
                break;
            }
            _ => {} // axum handles ping/pong automatically
        }
    }

    // The bus removes the subscription at its next publish or subscribe.
    drop(listener);
    send_task.abort();
    tracing::info!("WebSocket client disconnected");
}

struct WebSocketEventForwarder {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl EventListener for WebSocketEventForwarder {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        let _ = self.tx.send(event.clone());
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod router_surface_tests {
    //! What [`build_router`] exposes.
    //!
    //! The control plane is observation-shaped, so the router must expose no
    //! route that mutates the graph — a node's graph comes from its code. And
    //! no route asks for a credential: file permission on the local API socket
    //! is the whole gate.
    //!
    //! The router is the real one; only the `RuntimeOperations` backend is a
    //! stub.

    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use streamlib::sdk::descriptors::{
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;
    use streamlib::sdk::runtime::BoxFuture;
    use tower::ServiceExt;

    /// Stub runtime backing the router tests: it answers the observation ops.
    ///
    /// Every graph-mutating op is `unreachable!`. `RuntimeOperations` still
    /// declares them — the runtime API is not what changed — but no route may
    /// reach one, so a route that regrows here fails loudly instead of quietly
    /// succeeding against a permissive stub.
    #[derive(Clone, Default)]
    struct ControlPlaneRouterStubRuntime;

    impl RuntimeOperations for ControlPlaneRouterStubRuntime {
        fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn to_json(&self) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
        crate::control_plane_stub_support::the_exposed_port_subscription_is_unreachable!("route");
        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
    }

    crate::control_plane_stub_support::a_stub_runtime_loading_this_stub_as_its_only_stream!(
        ControlPlaneRouterStubRuntime
    );

    /// The routes this control plane deliberately does not have: every graph
    /// mutation the pre-pivot api-server served. Method + path exactly as they
    /// were, so this reads as the inventory it is.
    const DELETED_GRAPH_MUTATION_ROUTES: &[(&str, &str)] = &[
        ("POST", "/api/processor"),
        ("POST", "/api/processor/source"),
        ("POST", "/api/processor/source/replace"),
        ("DELETE", "/api/processors/some-id"),
        ("POST", "/api/connections"),
        ("DELETE", "/api/connections/some-id"),
    ];

    fn control_plane_router_over(runtime: ControlPlaneRouterStubRuntime) -> Router {
        build_router(
            Arc::new(runtime),
            crate::control_plane_stub_support::RelayInstancesOfThisMachineStub::shared(),
            CancellationToken::new(),
        )
    }

    /// A default stub runtime, for tests that build the router themselves.
    pub(crate) fn a_control_plane_router_stub_runtime()
    -> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
        Arc::new(ControlPlaneRouterStubRuntime)
    }

    /// The real router over a default stub runtime.
    pub(crate) fn control_plane_router_over_a_stub_runtime() -> Router {
        control_plane_router_over(ControlPlaneRouterStubRuntime)
    }

    async fn status_of(request: Request<Body>) -> StatusCode {
        control_plane_router_over_a_stub_runtime()
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    async fn json_body_of(request: Request<Body>) -> serde_json::Value {
        let response = control_plane_router_over_a_stub_runtime()
            .oneshot(request)
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The load-bearing surface assertion: no graph-mutation route is served.
    #[tokio::test]
    async fn the_router_serves_no_graph_mutation_route() {
        for (method, uri) in DELETED_GRAPH_MUTATION_ROUTES {
            let request = Request::builder()
                .method(*method)
                .uri(*uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let status = status_of(request).await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri} must not be routed; got {status}"
            );
        }
    }

    /// The spec is the contract a generated client is built from, so a mutation
    /// route must be absent from it too — not merely unrouted at runtime.
    #[tokio::test]
    async fn the_openapi_spec_documents_no_graph_mutation_route() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let spec = json_body_of(request).await;
        let paths = &spec["paths"];

        // Positive control. Indexing a `Value` yields `Null` for a missing key
        // AND for indexing a non-object, so the absence assertions below would
        // pass vacuously against an empty or malformed spec.
        assert!(
            paths["/api/graph"]["get"].is_object(),
            "the spec must still document the observation routes: {spec}"
        );

        for (_, uri) in DELETED_GRAPH_MUTATION_ROUTES {
            // The path-templated routes are documented under their template,
            // not the concrete id the runtime check uses.
            let documented = uri.replace("/some-id", "/{id}");
            assert!(
                paths[documented.as_str()].is_null() && paths[*uri].is_null(),
                "{documented} must not appear in the OpenAPI spec"
            );
        }
    }

    /// A generated client names a stream through the `stream` query parameter,
    /// optional where its absence reads every loaded stream.
    #[tokio::test]
    async fn the_openapi_spec_declares_the_stream_query_optional_on_reads_of_every_stream() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let spec = json_body_of(request).await;

        for path in ["/api/graph", "/api/registry"] {
            let parameters = spec["paths"][path]["get"]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{path} must document its parameters: {spec}"));
            let stream_parameter = parameters
                .iter()
                .find(|parameter| parameter["name"] == "stream")
                .unwrap_or_else(|| panic!("{path} must declare `stream`: {parameters:?}"));
            assert_eq!(
                stream_parameter["in"], "query",
                "{path}: {stream_parameter}"
            );
            assert_ne!(
                stream_parameter["required"],
                serde_json::json!(true),
                "{path}: {stream_parameter}"
            );
        }
    }

    /// The machine-wide answers are documented under the types the schema
    /// generator writes, so a generated client parses both forms.
    #[test]
    fn the_openapi_spec_documents_both_forms_of_the_graph_and_registry_answers() {
        let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
        let schemas = &spec["components"]["schemas"];
        for (answer, one_stream, every_stream) in [
            (
                "GraphOfOneStreamOrOfEveryStream",
                "GraphResponse",
                "MachineWideGraphResponse",
            ),
            (
                "NodeTypesOfOneStreamOrOfEveryStream",
                "RegistryResponse",
                "MachineWideRegistryResponse",
            ),
        ] {
            let rendered = schemas[answer].to_string();
            assert!(
                rendered.contains(one_stream) && rendered.contains(every_stream),
                "{answer}: {rendered}"
            );
            assert!(schemas[every_stream].is_object(), "{every_stream}: {spec}");
        }
    }

    /// Without `stream`, `/api/graph` renders every loaded stream under the
    /// machine's name, in the shape the schema generator documents.
    #[tokio::test]
    async fn the_graph_route_without_a_stream_renders_every_loaded_stream_under_the_machines_name()
    {
        let request = Request::builder()
            .method("GET")
            .uri("/api/graph")
            .body(Body::empty())
            .unwrap();
        let every_stream = json_body_of(request).await;

        assert_eq!(
            every_stream["machine"],
            crate::control_plane_stub_support::STUB_MACHINE_NAME
        );
        assert_eq!(
            every_stream["streams"].as_array().map(Vec::len),
            Some(1),
            "{every_stream}"
        );
    }

    /// The stub stream's node catalog is the process-global native registry,
    /// so this registers a probe under a path no other test names and asserts
    /// on that path alone.
    /// There is no teardown: a registration is for the life of the process, and
    /// the registry refuses a second one of the same path.
    ///
    /// `/api/registry` is where an agent learns which keys a node type's
    /// config takes, so it serves the descriptor's schema document itself —
    /// each field's type, its description and its default — rather than a
    /// name the agent would have to look up somewhere the node does not serve.
    #[tokio::test]
    async fn the_registry_serves_a_registered_node_types_config_schema_document() {
        let config_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
                "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
            },
            "required": [],
        });
        let class_import_path = "streamlib_api_server::registry_rendering_probe::TestPatternProbe";
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("TestPatternProbe").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "a registry-rendering probe",
                )
                .with_config_schema(config_schema.clone()),
            )
            .expect("the probe's path is registered by this test alone");

        for uri in [
            "/api/registry".to_string(),
            format!(
                "/api/registry?stream={}",
                crate::control_plane_stub_support::STUB_STREAM_ID
            ),
        ] {
            let request = Request::builder()
                .method("GET")
                .uri(&uri)
                .body(Body::empty())
                .unwrap();
            let served = json_body_of(request).await;

            let probe = served["nodes"]
                .as_array()
                .expect("a node type list")
                .iter()
                .find(|entry| entry["type"] == class_import_path)
                .unwrap_or_else(|| panic!("{uri}: the probe the test registered"));
            assert_eq!(probe["config_schema"], config_schema, "{uri}");
        }
    }

    /// The spec a client is generated from and the spec the node serves must be
    /// one document. They were two hand-maintained declarations once; the copy
    /// drifted, kept publishing routes the server had dropped, and nothing went
    /// red because each test read its own side.
    #[tokio::test]
    async fn the_generated_spec_and_the_served_spec_are_the_same_document() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let served = json_body_of(request).await;
        let generated = serde_json::to_value(control_plane_openapi_spec())
            .expect("the generated spec serializes");
        assert_eq!(
            served, generated,
            "`generate_openapi` and the served `/api/openapi.json` must not diverge"
        );
    }

    #[tokio::test]
    async fn the_observation_routes_answer_ok() {
        for uri in [
            "/health",
            "/api/graph",
            "/api/graph?stream=stub-stream",
            "/api/registry",
            "/api/registry?stream=stub-stream",
            "/api/openapi.json",
        ] {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_of(request).await,
                StatusCode::OK,
                "GET {uri} must answer 200"
            );
        }
    }

    /// A route reading one stream refuses a stream the runtime does not load
    /// with `404`, naming the streams it does.
    #[tokio::test]
    async fn a_route_naming_a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
        for uri in [
            "/api/graph?stream=elsewhere",
            "/api/registry?stream=elsewhere",
        ] {
            let response = control_plane_router_over_a_stub_runtime()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {uri}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let refusal = body["error"].as_str().unwrap_or_default();
            assert!(
                refusal.contains("elsewhere")
                    && refusal.contains(crate::control_plane_stub_support::STUB_STREAM_ID),
                "GET {uri}: {refusal}"
            );
        }
    }

    /// File permission on the local API socket is the whole gate, so a request
    /// carrying no credential reaches every route rather than a 401 or 403.
    /// `/mcp` is reached the same way by every MCP wire test's client.
    #[tokio::test]
    async fn no_route_asks_for_a_credential() {
        let requests_carrying_no_credential = [
            ("GET", "/api/graph".to_string(), ""),
            (
                "GET",
                format!(
                    "/api/registry?stream={}",
                    crate::control_plane_stub_support::STUB_STREAM_ID
                ),
                "",
            ),
        ];
        for (method, uri, body) in requests_carrying_no_credential {
            let request = Request::builder()
                .method(method)
                .uri(&uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            let status = status_of(request).await;
            assert!(
                ![
                    StatusCode::UNAUTHORIZED,
                    StatusCode::FORBIDDEN,
                    StatusCode::NOT_FOUND
                ]
                .contains(&status),
                "{method} {uri} must reach its handler with no credential; got {status}"
            );
        }
    }

    /// The runtime stops as a service does, or by a signal in its terminal:
    /// the spec documents reads alone, every operation a `GET`, and names no
    /// type that stops it.
    #[test]
    fn no_documented_route_shuts_the_runtime_down() {
        let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
        let mut documented_operations: Vec<(String, String)> = spec["paths"]
            .as_object()
            .expect("the spec documents its paths")
            .iter()
            .flat_map(|(path, path_item)| {
                path_item
                    .as_object()
                    .expect("each path documents its operations")
                    .keys()
                    .filter(|key| {
                        [
                            "get", "put", "post", "delete", "patch", "options", "head", "trace",
                        ]
                        .contains(&key.as_str())
                    })
                    .map(move |method| (path.clone(), method.clone()))
            })
            .collect();
        documented_operations.sort_unstable();

        assert_eq!(
            documented_operations,
            [
                ("/api/graph", "get"),
                ("/api/registry", "get"),
                ("/api/relay-adapters", "get"),
                ("/health", "get"),
            ]
            .map(|(path, method)| (path.to_owned(), method.to_owned()))
        );
        assert!(
            !spec.to_string().contains("Shutdown"),
            "the spec names no shutdown type"
        );
    }

    /// The router itself, not only its spec, serves nothing that changes the
    /// runtime: every path it serves outside `/mcp` (whose tools
    /// `tools_list_advertises_exactly_the_control_vocabulary` holds) answers
    /// no mutating method with success, and nothing at all is served under
    /// `/api/runtime`.
    #[tokio::test]
    async fn the_router_serves_no_mutating_method_and_nothing_under_api_runtime() {
        let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
        let documented_paths = spec["paths"]
            .as_object()
            .expect("the spec documents its paths")
            .keys()
            .map(|path| {
                format!(
                    "{}?stream={}",
                    path,
                    crate::control_plane_stub_support::STUB_STREAM_ID
                )
            });
        let routes_outside_the_spec = [
            "/ws/events",
            "/api/openapi.json",
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
        ]
        .map(str::to_owned);
        let served_paths: Vec<String> = documented_paths.chain(routes_outside_the_spec).collect();
        for path in &served_paths {
            for method in ["POST", "PUT", "PATCH", "DELETE"] {
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                let status = status_of(request).await;
                assert!(
                    !status.is_success(),
                    "{method} {path} must not be served; got {status}"
                );
            }
        }

        for path in [
            "/api/runtime",
            "/api/runtime/",
            "/api/runtime/stop",
            "/api/runtime/quit",
            "/api/runtime/exit",
            "/api/runtime/terminate",
        ] {
            for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                assert_eq!(
                    status_of(request).await,
                    StatusCode::NOT_FOUND,
                    "{method} {path} must not be served"
                );
            }
        }
    }
}

#[cfg(test)]
mod control_plane_request_trace_level_tests {
    //! The log level a routine control-plane request speaks at.
    //!
    //! At the engine's default `info` filter a node's control plane adds
    //! nothing to the app's own log. The trace is levelled, not deleted, so
    //! `RUST_LOG=tower_http=debug` brings all three records back.

    use super::router_surface_tests::control_plane_router_over_a_stub_runtime;
    use super::*;
    use crate::control_plane_stub_support::CapturedTracingTargets;
    use axum::body::Body;
    use axum::http::Request;
    use serial_test::serial;
    use tower::ServiceExt;

    fn serve_one_graph_request() {
        let request_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds");
        request_runtime.block_on(async {
            let response = control_plane_router_over_a_stub_runtime()
                .oneshot(
                    Request::builder()
                        .uri("/api/graph")
                        .body(Body::empty())
                        .expect("the graph request builds"),
                )
                .await
                .expect("the router answers");
            assert_eq!(response.status(), StatusCode::OK);
        });
    }

    fn tower_http_trace_targets_under(env_filter_directives: &str) -> Vec<&'static str> {
        CapturedTracingTargets::captured_from_the_second_of_two_runs(
            env_filter_directives,
            serve_one_graph_request,
        )
        .into_iter()
        .filter(|target| target.starts_with("tower_http"))
        .collect()
    }

    #[test]
    #[serial]
    fn a_routine_request_says_nothing_at_the_default_info_filter() {
        let targets = tower_http_trace_targets_under("info");
        assert!(
            targets.is_empty(),
            "a request must be silent at the engine's default filter, got: {targets:?}"
        );
    }

    #[test]
    #[serial]
    fn the_same_request_traces_all_three_hooks_under_tower_http_debug() {
        let targets = tower_http_trace_targets_under("info,tower_http=debug");
        for hook in [
            "tower_http::trace::make_span",
            "tower_http::trace::on_request",
            "tower_http::trace::on_response",
        ] {
            assert!(
                targets.contains(&hook),
                "asking for the request trace must yield {hook}, got: {targets:?}"
            );
        }
    }
}
