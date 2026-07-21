// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! HTTP + WebSocket handlers backing the routes declared in [`crate::routes`].

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::Path,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::sync::Arc;
use streamlib::sdk::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::json_schema::{
    ProcessorDescriptorOutput, RegistryResponse, SchemaDescriptorOutput, SchemaIdentOutput,
    SemanticVersionOutput,
};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::pubsub::{topics, Event, EventListener, PUBSUB};
use streamlib::sdk::runtime::RuntimeOperations;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::auth::{ApiServerBearerToken, ForbiddenResponse, UnauthorizedResponse};
use crate::state::{
    ApiDoc, AppState, CreateConnectionRequest, CreateProcessorRequest, ErrorResponse, IdResponse,
    ProcessorNotFoundResponse, ProcessorPortNotFoundResponse, UnknownProcessorTypeResponse,
};

// ============================================================================
// Router Construction
// ============================================================================

/// Build the full router with shared state and trace layer attached.
///
/// The four mutating routes (`POST /api/processor`, `DELETE
/// /api/processors/{id}`, `POST /api/connections`, `DELETE
/// /api/connections/{id}`) sit behind the bearer-token auth middleware only
/// when `auth_token` is `Some` (auth opted in); with `None` — the
/// zero-ceremony default — they are open like every other route. The GET
/// routes, health check, WebSocket event stream, and OpenAPI spec are always
/// open. `route_layer` binds the auth layer to exactly the routes already on
/// the protected sub-router, so a later `merge` leaves the open routes ungated.
pub(crate) fn build_router(
    runtime: Arc<dyn RuntimeOperations>,
    auth_token: Option<ApiServerBearerToken>,
    #[cfg(feature = "moq")] runtime_id: String,
) -> Router {
    let mut protected = OpenApiRouter::new()
        .routes(routes!(create_processor))
        .routes(routes!(delete_processor))
        .routes(routes!(create_connection))
        .routes(routes!(delete_connection));
    if let Some(auth_token) = auth_token {
        protected = protected.route_layer(axum::middleware::from_fn_with_state(
            auth_token,
            crate::auth::require_bearer_token,
        ));
    }

    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .routes(routes!(get_graph))
        .routes(routes!(get_registry))
        .routes(routes!(list_schema_definitions))
        .routes(routes!(get_schema_definition))
        .merge(protected)
        .split_for_parts();

    let state = AppState {
        runtime,
        #[cfg(feature = "moq")]
        runtime_id,
        openapi,
    };

    // TraceLayer logs all HTTP requests with method, path, status, and latency.
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_request(DefaultOnRequest::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    let router = router
        .route("/ws/events", get(websocket_handler))
        .route("/api/openapi.json", get(get_openapi_spec));

    #[cfg(feature = "moq")]
    let router = router.route("/api/moq/catalog", get(get_moq_catalog));

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

#[utoipa::path(
    get,
    path = "/api/graph",
    tag = "graph",
    responses(
        (status = 200, description = "Current graph state as JSON"),
        (status = 500, description = "Internal server error")
    )
)]
pub(crate) async fn get_graph(
    State(state): State<AppState>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    state
        .runtime
        .to_json_async()
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[utoipa::path(
    post,
    path = "/api/processor",
    tag = "processors",
    request_body = CreateProcessorRequest,
    responses(
        (status = 200, description = "Processor created successfully", body = IdResponse),
        (status = 400, description = "Malformed request (invalid org / package / type / version segment)", body = ErrorResponse),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 422, description = "Processor type is structurally valid but not registered in the runtime; the failed node is left in the graph in `Error` state", body = UnknownProcessorTypeResponse)
    )
)]
pub(crate) async fn create_processor(
    State(state): State<AppState>,
    Json(body): Json<CreateProcessorRequest>,
) -> axum::response::Response {
    // Convert SchemaIdentOutput → SchemaIdent through the typed segment
    // validators (Org::new / Package::new / TypeName::new / SemVer::new).
    // This is typed conversion, not parsing — there is no `SchemaIdent::parse`.
    let SchemaIdentOutput {
        org,
        package,
        type_name,
        version,
    } = body.processor_type.clone();
    let ident = match (
        Org::new(org),
        Package::new(package),
        TypeName::new(type_name),
    ) {
        (Ok(org), Ok(package), Ok(type_name)) => SchemaIdent::new(
            org,
            package,
            type_name,
            SemVer::new(version.major, version.minor, version.patch),
        ),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Malformed processor identifier — one of org / package / type failed validation".into(),
                }),
            )
                .into_response();
        }
    };
    let spec = ProcessorSpec::new(ident, body.config);

    match state.runtime.add_processor_async(spec).await {
        Ok(id) => (
            StatusCode::OK,
            Json(IdResponse { id: id.to_string() }),
        )
            .into_response(),
        Err(Error::UnknownProcessorType { ident: _ }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(UnknownProcessorTypeResponse {
                error: "UnknownProcessorType",
                ident: body.processor_type,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/api/processors/{id}",
    tag = "processors",
    params(
        ("id" = String, Path, description = "Processor ID to delete")
    ),
    responses(
        (status = 204, description = "Processor deleted successfully"),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 404, description = "Processor not found")
    )
)]
pub(crate) async fn delete_processor(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let processor_id = id.into();
    state
        .runtime
        .remove_processor_async(processor_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

#[utoipa::path(
    post,
    path = "/api/connections",
    tag = "connections",
    request_body = CreateConnectionRequest,
    responses(
        (status = 200, description = "Connection created successfully", body = IdResponse),
        (status = 400, description = "Malformed request or generic graph error", body = ErrorResponse),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 404, description = "One of the referenced processors isn't in the graph", body = ProcessorNotFoundResponse),
        (status = 422, description = "Referenced processor exists but has no port with that name and direction", body = ProcessorPortNotFoundResponse)
    )
)]
pub(crate) async fn create_connection(
    State(state): State<AppState>,
    Json(body): Json<CreateConnectionRequest>,
) -> axum::response::Response {
    let from = OutputLinkPortRef::new(body.from_processor, body.from_port);
    let to = InputLinkPortRef::new(body.to_processor, body.to_port);

    match state.runtime.connect_async(from, to).await {
        Ok(id) => (
            StatusCode::OK,
            Json(IdResponse { id: id.to_string() }),
        )
            .into_response(),
        Err(Error::ProcessorNotFound(processor_id)) => (
            StatusCode::NOT_FOUND,
            Json(ProcessorNotFoundResponse {
                error: "ProcessorNotFound",
                processor_id,
            }),
        )
            .into_response(),
        Err(Error::ProcessorPortNotFound {
            processor_id,
            port_name,
            direction,
        }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ProcessorPortNotFoundResponse {
                error: "ProcessorPortNotFound",
                processor_id,
                port_name,
                direction: match direction {
                    streamlib::sdk::error::PortDirection::Input => "input",
                    streamlib::sdk::error::PortDirection::Output => "output",
                },
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/api/connections/{id}",
    tag = "connections",
    params(
        ("id" = String, Path, description = "Connection ID to delete")
    ),
    responses(
        (status = 204, description = "Connection deleted successfully"),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 404, description = "Connection not found")
    )
)]
pub(crate) async fn delete_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let link_id = id.into();

    state
        .runtime
        .disconnect_async(link_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    responses(
        (status = 200, description = "Available processors and schemas", body = RegistryResponse)
    )
)]
pub(crate) async fn get_registry() -> Json<RegistryResponse> {
    let processors: Vec<ProcessorDescriptorOutput> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .map(|d| ProcessorDescriptorOutput::from(&d))
        .collect();

    let schemas: Vec<SchemaDescriptorOutput> = PROCESSOR_REGISTRY
        .known_schemas()
        .into_iter()
        .map(|spec| SchemaDescriptorOutput {
            name: spec.to_string(),
            version: SemanticVersionOutput {
                major: 1,
                minor: 0,
                patch: 0,
            },
            fields: vec![],
            read_behavior: Default::default(),
            default_capacity: 0,
        })
        .collect();

    Json(RegistryResponse {
        processors,
        schemas,
    })
}

#[utoipa::path(
    get,
    path = "/api/schemas",
    tag = "schemas",
    responses(
        (status = 200, description = "List of canonical schema identifiers currently registered with the runtime", body = Vec<String>)
    )
)]
pub(crate) async fn list_schema_definitions() -> Json<Vec<String>> {
    Json(streamlib::sdk::schemas::current_schema_idents())
}

#[utoipa::path(
    get,
    path = "/api/schemas/{name}",
    tag = "schemas",
    params(
        ("name" = String, Path, description = "Schema name (e.g. @tatolab/core/VideoFrame)")
    ),
    responses(
        (status = 200, description = "YAML schema definition", body = String),
        (status = 404, description = "Schema not found")
    )
)]
pub(crate) async fn get_schema_definition(
    Path(name): Path<String>,
) -> std::result::Result<String, axum::http::StatusCode> {
    streamlib::sdk::schemas::current_schema_definition(&name)
        .map(|def| def.to_string())
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

pub(crate) async fn get_openapi_spec(
    State(state): State<AppState>,
) -> Json<utoipa::openapi::OpenApi> {
    Json(state.openapi)
}

/// MoQ broadcast catalog with currently-published tracks.
///
/// Returns an empty catalog when no MoQ publish processor has touched this
/// runtime yet — the package-global session registry in `@tatolab/moq` is
/// populated lazily on first publish.
#[cfg(feature = "moq")]
pub(crate) async fn get_moq_catalog(
    State(state): State<AppState>,
) -> Json<streamlib_moq::MoqBroadcastCatalog> {
    let mut catalog = streamlib_moq::MoqBroadcastCatalog::new();
    if let Some(sessions) = streamlib_moq::try_sessions_for_runtime(&state.runtime_id) {
        for track_name in sessions.published_track_names() {
            catalog.add_track(&track_name, None, None, &track_name);
        }
    }
    Json(catalog)
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

    // Listener that forwards events to channel
    let listener = Arc::new(Mutex::new(WebSocketEventForwarder { tx }));

    // Subscribe to ALL topics via wildcard
    PUBSUB.subscribe(topics::ALL, listener.clone());

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

    // Cleanup
    drop(listener); // Weak ref cleanup on next publish
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
mod router_auth_gate_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        Request, StatusCode,
    };
    use streamlib::sdk::descriptors::{ModuleIdent, SemVerRange};
    use streamlib::sdk::graph::{LinkUniqueId, ProcessorUniqueId};
    use streamlib::sdk::runtime::{
        BoxFuture, RegisterProcessorReceipt, ReplaceProcessorFromSource, SubmittedProcessorSource,
    };
    use tower::ServiceExt;

    /// Stub runtime whose graph mutations all succeed, so the REAL
    /// [`build_router`] auth gate can be exercised end-to-end. With auth
    /// enabled, a mutating handler reaches its `Ok` result (200 / 204) only
    /// once the bearer-token middleware has admitted the request — deleting the
    /// `route_layer` gate flips the missing-token cases from 401 to those
    /// success codes, so the enabled-mode tests go red on that regression. With
    /// auth off (the default), the same handlers must be reachable with no
    /// token at all.
    struct AlwaysOkStubRuntime;

    impl RuntimeOperations for AlwaysOkStubRuntime {
        fn add_processor_async(
            &self,
            _spec: ProcessorSpec,
        ) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
            Box::pin(async { Ok(ProcessorUniqueId::new()) })
        }
        fn remove_processor_async(
            &self,
            _processor_id: ProcessorUniqueId,
        ) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn connect_async(
            &self,
            _from: OutputLinkPortRef,
            _to: InputLinkPortRef,
        ) -> BoxFuture<'_, Result<LinkUniqueId>> {
            Box::pin(async { Ok(LinkUniqueId::new()) })
        }
        fn disconnect_async(&self, _link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn register_processor_source_async(
            &self,
            _request: SubmittedProcessorSource,
        ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>> {
            Box::pin(async { Ok(stub_register_receipt()) })
        }
        fn replace_processor_async(
            &self,
            _request: ReplaceProcessorFromSource,
        ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>> {
            Box::pin(async { Ok(stub_register_receipt()) })
        }
        fn add_processor(&self, _spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
            Ok(ProcessorUniqueId::new())
        }
        fn remove_processor(&self, _processor_id: &ProcessorUniqueId) -> Result<()> {
            Ok(())
        }
        fn connect(&self, _from: OutputLinkPortRef, _to: InputLinkPortRef) -> Result<LinkUniqueId> {
            Ok(LinkUniqueId::new())
        }
        fn disconnect(&self, _link_id: &LinkUniqueId) -> Result<()> {
            Ok(())
        }
        fn to_json(&self) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }

    /// A minimal always-succeeds register/replace receipt for [`AlwaysOkStubRuntime`]:
    /// a dummy `@session/stub` registration ident with no installed processors.
    /// The register-from-source path is not exercised by the auth-gate tests, so
    /// the port surface is empty.
    fn stub_register_receipt() -> RegisterProcessorReceipt {
        RegisterProcessorReceipt::new(
            ModuleIdent::new(
                Org::new("session").expect("session org passes the org grammar"),
                Package::new("stub").expect("stub package passes the package grammar"),
                SemVerRange::Exact(SemVer::new(0, 0, 0)),
            ),
            vec![],
        )
    }

    const TEST_TOKEN: &str = "test-bearer-secret";

    /// Router with bearer auth explicitly enabled — the mutating routes are
    /// gated behind [`TEST_TOKEN`].
    fn auth_enabled_router() -> Router {
        build_router(
            Arc::new(AlwaysOkStubRuntime),
            Some(ApiServerBearerToken::from_secret(TEST_TOKEN)),
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
        )
    }

    /// Router in the default (auth-off) mode — every route, including the
    /// mutating ones, is open with no token.
    fn auth_disabled_router() -> Router {
        build_router(
            Arc::new(AlwaysOkStubRuntime),
            None,
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
        )
    }

    async fn status_on(router: Router, request: Request<Body>) -> StatusCode {
        router.oneshot(request).await.unwrap().status()
    }

    async fn status_of(request: Request<Body>) -> StatusCode {
        status_on(auth_enabled_router(), request).await
    }

    fn create_processor_body() -> Body {
        Body::from(
            serde_json::json!({
                "processor_type": {
                    "org": "tatolab",
                    "package": "debug-utilities",
                    "type": "SimplePassthroughProcessor",
                    "version": { "major": 1, "minor": 0, "patch": 0 }
                },
                "config": {}
            })
            .to_string(),
        )
    }

    fn create_connection_body() -> Body {
        Body::from(
            serde_json::json!({
                "from_processor": "p1",
                "from_port": "output",
                "to_processor": "p2",
                "to_port": "input"
            })
            .to_string(),
        )
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[tokio::test]
    async fn mutating_routes_reject_missing_token_with_401() {
        let unauthenticated = [
            Request::builder()
                .method("POST")
                .uri("/api/processor")
                .header(CONTENT_TYPE, "application/json")
                .body(create_processor_body())
                .unwrap(),
            Request::builder()
                .method("POST")
                .uri("/api/connections")
                .header(CONTENT_TYPE, "application/json")
                .body(create_connection_body())
                .unwrap(),
            Request::builder()
                .method("DELETE")
                .uri("/api/processors/some-id")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method("DELETE")
                .uri("/api/connections/some-id")
                .body(Body::empty())
                .unwrap(),
        ];
        for request in unauthenticated {
            assert_eq!(status_of(request).await, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn create_processor_with_token_is_200() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/processor")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(create_processor_body())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn create_connection_with_token_is_200() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/connections")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(create_connection_body())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_processor_with_token_is_204() {
        let request = Request::builder()
            .method("DELETE")
            .uri("/api/processors/some-id")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_connection_with_token_is_204() {
        let request = Request::builder()
            .method("DELETE")
            .uri("/api/connections/some-id")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn open_routes_need_no_authorization_header() {
        let open = ["/health", "/api/registry", "/api/openapi.json"];
        for uri in open {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_of(request).await,
                StatusCode::OK,
                "GET {uri} must stay open (no bearer token)"
            );
        }
    }

    #[tokio::test]
    async fn auth_off_lets_create_routes_through_without_a_token() {
        // The zero-ceremony default: with auth off, the mutating POST routes
        // reach their handlers (200) with no `Authorization` header. Reapplying
        // the gate unconditionally in `build_router` flips these to 401.
        let posts = [
            ("/api/processor", create_processor_body()),
            ("/api/connections", create_connection_body()),
        ];
        for (uri, body) in posts {
            let request = Request::builder()
                .method("POST")
                .uri(uri)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap();
            assert_eq!(
                status_on(auth_disabled_router(), request).await,
                StatusCode::OK,
                "POST {uri} must be open with auth off (no token)"
            );
        }
    }

    #[tokio::test]
    async fn auth_off_lets_delete_routes_through_without_a_token() {
        let deletes = ["/api/processors/some-id", "/api/connections/some-id"];
        for uri in deletes {
            let request = Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_on(auth_disabled_router(), request).await,
                StatusCode::NO_CONTENT,
                "DELETE {uri} must be open with auth off (no token)"
            );
        }
    }
}
