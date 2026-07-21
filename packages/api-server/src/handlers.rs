// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! HTTP + WebSocket handlers, wired into the router by [`build_router`].

use axum::{
    Json, Router,
    extract::Path,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::sync::Arc;
use streamlib::sdk::descriptors::{
    ModuleIdent, Org, Package, SchemaIdent, SemVer, SemVerRange, TypeName,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::json_schema::{
    ProcessorDescriptorOutput, RegistryResponse, SchemaDescriptorOutput, SchemaIdentOutput,
    SemanticVersionOutput,
};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::{
    RegisterProcessorReceipt, ReplaceProcessorFromSource, RuntimeOperations,
    SubmittedProcessorSource,
};
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::auth::{ApiServerBearerToken, ForbiddenResponse, UnauthorizedResponse};
use crate::state::{
    ApiDoc, AppState, CreateConnectionRequest, CreateProcessorRequest, ErrorResponse, IdResponse,
    ProcessorNotFoundResponse, ProcessorPortNotFoundResponse, RegisterProcessorSourceResponse,
    RegisteredPortResponse, RegisteredProcessorPortsResponse, ReplaceProcessorSourceRequest,
    SourceProcessorPortRole, SubmittedProcessorSourceRequest, UnknownProcessorTypeResponse,
};

/// The relative WebSocket URL carrying this runtime's live event stream — the
/// route registered in [`build_router`]. Returned in the source-submit
/// response so a client learns where to observe the new instance's live state.
const RUNTIME_EVENTS_URL: &str = "/ws/events";

// ============================================================================
// Router Construction
// ============================================================================

/// Build the full router with shared state and trace layer attached.
///
/// The mutating routes (`POST /api/processor`, `POST /api/processor/source`,
/// `POST /api/processor/source/replace`, `DELETE /api/processors/{id}`, `POST
/// /api/connections`, `DELETE /api/connections/{id}`) sit behind the
/// bearer-token auth middleware only when `auth_token` is `Some` (auth opted
/// in); with `None` — the zero-ceremony default — they are open like every
/// other route. The two source-submit routes are RCE-capable (they execute
/// submitted source), so they join this gated group. The GET routes, health
/// check, WebSocket event stream, and OpenAPI spec are always open.
/// `route_layer` binds the auth layer to exactly the routes already on the
/// protected sub-router, so a later `merge` leaves the open routes ungated.
pub(crate) fn build_router(
    runtime: Arc<dyn RuntimeOperations>,
    auth_token: Option<ApiServerBearerToken>,
    #[cfg(feature = "moq")] runtime_id: String,
) -> Router {
    let mut protected = OpenApiRouter::new()
        .routes(routes!(create_processor))
        .routes(routes!(create_processor_source))
        .routes(routes!(replace_processor_source))
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
        Ok(id) => (StatusCode::OK, Json(IdResponse { id: id.to_string() })).into_response(),
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

/// Project a register/replace receipt's committed ports onto the wire
/// response shape (`schema` rendered as `"any"` or `@org/package/Type@version`).
fn project_receipt_ports(
    receipt: &RegisterProcessorReceipt,
) -> Vec<RegisteredProcessorPortsResponse> {
    let project = |ports: &[streamlib::sdk::runtime::RegisteredPortReceipt]| {
        ports
            .iter()
            .map(|port| RegisteredPortResponse {
                name: port.name.clone(),
                schema: port.schema.to_string(),
                delivery_profile: port.delivery_profile.clone(),
            })
            .collect()
    };
    receipt
        .processors
        .iter()
        .map(|processor| RegisteredProcessorPortsResponse {
            name: processor.name.clone(),
            inputs: project(&processor.inputs),
            outputs: project(&processor.outputs),
        })
        .collect()
}

/// The concrete [`SemVer`] a session-module range pins. Session registrations
/// mint an `Exact` range, so the other range shapes fall back to their lower
/// bound and only a wildcard `Any` (never minted for a session) yields `None`.
fn pinned_version(range: &SemVerRange) -> Option<SemVer> {
    match range {
        SemVerRange::Exact(version)
        | SemVerRange::AtLeast(version)
        | SemVerRange::Caret(version)
        | SemVerRange::Tilde(version) => Some(version.clone()),
        SemVerRange::Any => None,
    }
}

/// Build the instantiable [`SchemaIdent`] for a discovered processor `type_name`
/// under the receipt's minted `@org/name@0.0.N` registration module.
fn session_processor_ident(module: &ModuleIdent, type_name: &str) -> Option<SchemaIdent> {
    let r#type = TypeName::new(type_name.to_string()).ok()?;
    let version = pinned_version(&module.version)?;
    Some(SchemaIdent::new(
        module.org.clone(),
        module.name.clone(),
        r#type,
        version,
    ))
}

/// Map a register/replace-from-source [`Error`] onto an HTTP response. The
/// source-submit refusals (unsupported language, missing name, un-mintable
/// name, build failure, replace-target mismatch) surface as
/// [`Error::Configuration`] — the JSON request was well-formed but the
/// submitted source could not be registered — so they map to 422; a runtime
/// failure (including a catastrophic replace where restoring the prior
/// registration also failed) maps to 500.
fn source_submit_error_response(error: Error) -> axum::response::Response {
    let status = match error {
        Error::Configuration(_) => StatusCode::UNPROCESSABLE_ENTITY,
        Error::Runtime(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[utoipa::path(
    post,
    path = "/api/processor/source",
    tag = "processors",
    request_body = SubmittedProcessorSourceRequest,
    responses(
        (status = 200, description = "Source registered, first discovered processor instantiated, and optional connections wired; body carries the minted registration ident, discovered ports, instance id, and connection ids", body = RegisterProcessorSourceResponse),
        (status = 400, description = "A referenced peer processor/port for a `connect` wiring is malformed", body = ErrorResponse),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 404, description = "A `connect` wiring references a peer processor not in the graph", body = ProcessorNotFoundResponse),
        (status = 422, description = "The submitted source could not be registered or instantiated (unsupported language, missing name, build failure, or unknown processor type)", body = ErrorResponse),
        (status = 500, description = "Runtime failure while registering the source", body = ErrorResponse)
    )
)]
pub(crate) async fn create_processor_source(
    State(state): State<AppState>,
    Json(body): Json<SubmittedProcessorSourceRequest>,
) -> axum::response::Response {
    let submitted = SubmittedProcessorSource {
        source_text: body.source,
        language: body.language.into(),
        requested_name: body.requested_name,
        processor_type_name: body.processor_type_name,
    };

    let receipt = match state
        .runtime
        .register_processor_source_async(submitted)
        .await
    {
        Ok(receipt) => receipt,
        Err(error) => return source_submit_error_response(error),
    };

    let processors = project_receipt_ports(&receipt);
    let module = receipt.module.to_string();

    // Composite "app is code" server-side wiring: instantiate the first
    // discovered processor, then apply the optional connect wirings.
    let Some(first) = receipt.processors.first() else {
        return (
            StatusCode::OK,
            Json(RegisterProcessorSourceResponse {
                module,
                processors,
                processor_id: None,
                state: "registered",
                connections: Vec::new(),
                events_url: RUNTIME_EVENTS_URL,
            }),
        )
            .into_response();
    };

    let Some(ident) = session_processor_ident(&receipt.module, &first.name) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: format!(
                    "registered module `{module}` yielded an uninstantiable processor identity for type `{}`",
                    first.name
                ),
            }),
        )
            .into_response();
    };

    let config = body.config.unwrap_or_else(|| serde_json::json!({}));
    let processor_id = match state
        .runtime
        .add_processor_async(ProcessorSpec::new(ident, config))
        .await
    {
        Ok(id) => id,
        Err(Error::UnknownProcessorType { ident: _ }) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!(
                        "registered module `{module}` did not expose processor type `{}` to the runtime",
                        first.name
                    ),
                }),
            )
                .into_response();
        }
        Err(error) => return source_submit_error_response(error),
    };

    let mut connections = Vec::with_capacity(body.connect.len());
    for wiring in body.connect {
        let (from, to) = match wiring.role {
            SourceProcessorPortRole::Output => (
                OutputLinkPortRef::new(processor_id.clone(), wiring.local_port),
                InputLinkPortRef::new(wiring.peer_processor, wiring.peer_port),
            ),
            SourceProcessorPortRole::Input => (
                OutputLinkPortRef::new(wiring.peer_processor, wiring.peer_port),
                InputLinkPortRef::new(processor_id.clone(), wiring.local_port),
            ),
        };
        match state.runtime.connect_async(from, to).await {
            Ok(link_id) => connections.push(link_id.to_string()),
            Err(Error::ProcessorNotFound(peer)) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ProcessorNotFoundResponse {
                        error: "ProcessorNotFound",
                        processor_id: peer,
                    }),
                )
                    .into_response();
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: error.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::OK,
        Json(RegisterProcessorSourceResponse {
            module,
            processors,
            processor_id: Some(processor_id.to_string()),
            state: "added",
            connections,
            events_url: RUNTIME_EVENTS_URL,
        }),
    )
        .into_response()
}

#[utoipa::path(
    post,
    path = "/api/processor/source/replace",
    tag = "processors",
    request_body = ReplaceProcessorSourceRequest,
    responses(
        (status = 200, description = "Prior `@session/<name>` registration replaced; body carries the new registration ident and discovered ports (type-level replacement — running graph instances are not swapped)", body = RegisterProcessorSourceResponse),
        (status = 400, description = "`target_session_module` is not a valid `@org/name@<range>` module ident", body = ErrorResponse),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 422, description = "The replacement source could not be registered, or its name does not resolve to the target's `@session/<name>`", body = ErrorResponse),
        (status = 500, description = "Runtime failure while replacing (including a replacement that failed and could not restore the prior registration)", body = ErrorResponse)
    )
)]
pub(crate) async fn replace_processor_source(
    State(state): State<AppState>,
    Json(body): Json<ReplaceProcessorSourceRequest>,
) -> axum::response::Response {
    use serde::{Deserialize, de::IntoDeserializer};
    let target_session_module: ModuleIdent = match ModuleIdent::deserialize(
        body.target_session_module.as_str().into_deserializer(),
    ) {
        Ok(module) => module,
        Err(error) => {
            let error: serde::de::value::Error = error;
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!(
                        "target_session_module `{}` is not a valid `@org/name@<range>` module ident: {error}",
                        body.target_session_module
                    ),
                }),
            )
                .into_response();
        }
    };

    let request = ReplaceProcessorFromSource {
        target_session_module,
        replacement: SubmittedProcessorSource {
            source_text: body.source,
            language: body.language.into(),
            requested_name: body.requested_name,
            processor_type_name: body.processor_type_name,
        },
    };

    match state.runtime.replace_processor_async(request).await {
        Ok(receipt) => (
            StatusCode::OK,
            Json(RegisterProcessorSourceResponse {
                module: receipt.module.to_string(),
                processors: project_receipt_ports(&receipt),
                processor_id: None,
                state: "registered",
                connections: Vec::new(),
                events_url: RUNTIME_EVENTS_URL,
            }),
        )
            .into_response(),
        Err(error) => source_submit_error_response(error),
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
        Ok(id) => (StatusCode::OK, Json(IdResponse { id: id.to_string() })).into_response(),
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
        Request, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    };
    use streamlib::sdk::descriptors::{ModuleIdent, SemVerRange};
    use streamlib::sdk::graph::{LinkUniqueId, ProcessorUniqueId};
    use streamlib::sdk::processors::PortSchemaSpec;
    use streamlib::sdk::runtime::{
        BoxFuture, RegisterProcessorReceipt, RegisteredPortReceipt, RegisteredProcessorReceipt,
        ReplaceProcessorFromSource, SubmittedProcessorSource,
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

    /// An always-succeeds register/replace receipt for [`AlwaysOkStubRuntime`]:
    /// a `@session/stub@0.0.0` registration installing one `Widget` processor
    /// with a `video` input (`any`) and a `frame` output (a specific
    /// `@tatolab/core/VideoFrame@1.0.0`). Non-empty so the source-submit
    /// composite reaches its instantiate step and the port projection is
    /// exercised; the auth-gate tests ignore the body.
    fn stub_register_receipt() -> RegisterProcessorReceipt {
        RegisterProcessorReceipt::new(
            ModuleIdent::new(
                Org::new("session").expect("session org passes the org grammar"),
                Package::new("stub").expect("stub package passes the package grammar"),
                SemVerRange::Exact(SemVer::new(0, 0, 0)),
            ),
            vec![RegisteredProcessorReceipt {
                name: "Widget".to_string(),
                inputs: vec![RegisteredPortReceipt {
                    name: "video".to_string(),
                    schema: PortSchemaSpec::Any,
                    delivery_profile: Some("latest".to_string()),
                }],
                outputs: vec![RegisteredPortReceipt {
                    name: "frame".to_string(),
                    schema: PortSchemaSpec::Specific(SchemaIdent::new(
                        Org::new("tatolab").expect("tatolab org passes the grammar"),
                        Package::new("core").expect("core package passes the grammar"),
                        TypeName::new("VideoFrame").expect("VideoFrame type name is valid"),
                        SemVer::new(1, 0, 0),
                    )),
                    delivery_profile: None,
                }],
            }],
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

    fn create_processor_source_body() -> Body {
        Body::from(
            serde_json::json!({
                "language": "python",
                "source": "class Widget:\n    pass\n",
                "requested_name": "widget"
            })
            .to_string(),
        )
    }

    fn replace_processor_source_body() -> Body {
        Body::from(
            serde_json::json!({
                "target_session_module": "@session/widget@*",
                "language": "python",
                "source": "class Widget:\n    pass\n",
                "requested_name": "widget"
            })
            .to_string(),
        )
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    async fn json_body_on(router: Router, request: Request<Body>) -> serde_json::Value {
        let response = router.oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
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
                .uri("/api/processor/source")
                .header(CONTENT_TYPE, "application/json")
                .body(create_processor_source_body())
                .unwrap(),
            Request::builder()
                .method("POST")
                .uri("/api/processor/source/replace")
                .header(CONTENT_TYPE, "application/json")
                .body(replace_processor_source_body())
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
    async fn create_processor_source_returns_discovered_ports_and_instance() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/processor/source")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(create_processor_source_body())
            .unwrap();
        let body = json_body_on(auth_enabled_router(), request).await;

        assert_eq!(body["module"], "@session/stub@=0.0.0");
        assert_eq!(body["state"], "added");
        assert!(
            body["processor_id"].is_string(),
            "the composite must instantiate the first discovered processor and return its id"
        );
        assert_eq!(body["events_url"], "/ws/events");

        let processors = body["processors"].as_array().expect("processors array");
        assert_eq!(processors.len(), 1);
        assert_eq!(processors[0]["name"], "Widget");
        assert_eq!(processors[0]["inputs"][0]["name"], "video");
        assert_eq!(processors[0]["inputs"][0]["schema"], "any");
        assert_eq!(processors[0]["inputs"][0]["delivery_profile"], "latest");
        assert_eq!(processors[0]["outputs"][0]["name"], "frame");
        assert_eq!(
            processors[0]["outputs"][0]["schema"],
            "@tatolab/core/VideoFrame@1.0.0"
        );
    }

    #[tokio::test]
    async fn create_processor_source_wires_optional_connections() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/processor/source")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "language": "python",
                    "source": "class Widget:\n    pass\n",
                    "requested_name": "widget",
                    "connect": [{
                        "local_port": "frame",
                        "role": "output",
                        "peer_processor": "display-1",
                        "peer_port": "video"
                    }]
                })
                .to_string(),
            ))
            .unwrap();
        let body = json_body_on(auth_enabled_router(), request).await;

        let connections = body["connections"].as_array().expect("connections array");
        assert_eq!(
            connections.len(),
            1,
            "the single requested wiring must produce one link id"
        );
        assert!(connections[0].is_string());
    }

    #[tokio::test]
    async fn replace_processor_source_with_token_is_200() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/processor/source/replace")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(replace_processor_source_body())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn replace_processor_source_rejects_malformed_target_module_with_400() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/processor/source/replace")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "target_session_module": "not-a-module-ident",
                    "language": "python",
                    "source": "class Widget:\n    pass\n",
                    "requested_name": "widget"
                })
                .to_string(),
            ))
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn source_routes_are_documented_in_the_openapi_spec() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let spec = json_body_on(auth_enabled_router(), request).await;
        let paths = &spec["paths"];
        assert!(
            paths["/api/processor/source"]["post"].is_object(),
            "POST /api/processor/source must appear in the OpenAPI spec"
        );
        assert!(
            paths["/api/processor/source/replace"]["post"].is_object(),
            "POST /api/processor/source/replace must appear in the OpenAPI spec"
        );
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
