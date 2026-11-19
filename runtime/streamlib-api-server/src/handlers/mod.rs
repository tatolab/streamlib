// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP handlers, wired into the router by [`build_router`].

use axum::{
    Json, Router,
    extract::Query,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use streamlib::sdk::descriptors::NodeDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::json_schema::{
    MachineWideRegistryResponse, NodeDescriptorOutput, NodeTypeAnInstalledModuleDeclaresOutput,
    NodeTypesDescribedInOneStreamOutput, PortFamilyDescriptorOutput, RegistryResponse,
};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamListing,
};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_JSON_RPC_ROUTE_PATH, CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
    MCP_STDIO_UPGRADE_REQUEST_TARGET, MCP_STREAMABLE_HTTP_ROUTE_PATH,
};

use crate::loopback_http_listener::ControlWebSocketRouteForTheLoopbackListener;
use crate::state::{ApiDoc, AppState, ErrorResponse, OptionalStreamSelectionQuery};

// ============================================================================
// Router Construction
// ============================================================================

/// The REST routes the control plane serves, each with its OpenAPI registration.
fn control_plane_rest_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(get_graph))
        .routes(routes!(get_registry))
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

pub(crate) fn build_router(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    local_api_stopping_token: CancellationToken,
) -> LocalApiRouters {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(control_plane_rest_routes())
        .split_for_parts();

    let local_api_mcp_server_handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::clone(&operations_on_the_loaded_streams),
        local_api_stopping_token.clone(),
    );
    let local_api_mcp_service =
        crate::mcp::local_api_mcp_streamable_http_service(local_api_mcp_server_handler.clone());
    let state = AppState {
        operations_on_the_loaded_streams,
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

    let control_operation_dispatcher = local_api_mcp_server_handler.control_operation_dispatcher();
    let control_websocket_route_for_the_loopback_listener =
        ControlWebSocketRouteForTheLoopbackListener::new(Router::new().route(
            CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
            crate::control_json_rpc_adapter::control_json_rpc_websocket_route(
                control_operation_dispatcher.clone(),
                Arc::clone(&local_api_mcp_server_handler.project_repositories_open_in_this_runtime),
                local_api_stopping_token.clone(),
            ),
        ));
    let router = router
        .route(
            CONTROL_JSON_RPC_ROUTE_PATH,
            crate::control_json_rpc_adapter::control_json_rpc_route(
                control_operation_dispatcher.clone(),
            ),
        )
        .route(
            CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
            crate::control_json_rpc_adapter::control_json_rpc_websocket_route(
                control_operation_dispatcher.clone(),
                Arc::clone(&local_api_mcp_server_handler.project_repositories_open_in_this_runtime),
                local_api_stopping_token.clone(),
            ),
        )
        .route("/api/openapi.json", get(get_openapi_spec))
        .route_service(MCP_STREAMABLE_HTTP_ROUTE_PATH, local_api_mcp_service)
        .route(
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
            crate::mcp_stdio_upgrade::local_api_mcp_stdio_upgrade_route(
                local_api_mcp_server_handler,
                local_api_stopping_token,
            ),
        );

    LocalApiRouters {
        local_api_socket_router: router.layer(trace_layer).with_state(state),
        control_websocket_route_for_the_loopback_listener,
    }
}

/// The routers one local API serves: everything on its socket, and its control WebSocket for
/// the loopback listener.
pub(crate) struct LocalApiRouters {
    pub(crate) local_api_socket_router: Router,
    pub(crate) control_websocket_route_for_the_loopback_listener:
        ControlWebSocketRouteForTheLoopbackListener,
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
/// `404` for one the route cannot resolve — the refusal names the streams it
/// could — `400` for a name that casts to nothing, and `500` for anything else.
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

/// What `GET /api/registry` answers: one stream's node catalog, loaded or not, or
/// the native types and each loaded stream's own.
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
        stream_graphs.push(stream_operations.the_graph_as_answered_async().await?);
    }
    give_the_stream_graphs_their_condition(operations_on_the_loaded_streams, &mut stream_graphs)
        .await?;
    Ok(json!({
        "machine": operations_on_the_loaded_streams.this_machines_name().name(),
        "streams": stream_graphs,
    }))
}

/// Write into each of `stream_graphs` its stream's condition, as the stream listing answers it.
pub(crate) async fn give_the_stream_graphs_their_condition(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    stream_graphs: &mut [Value],
) -> Result<()> {
    let operations_on_the_loaded_streams = Arc::clone(operations_on_the_loaded_streams);
    let listings =
        tokio::task::spawn_blocking(move || operations_on_the_loaded_streams.list_streams())
            .await
            .map_err(|not_finished| {
                Error::Runtime(format!("the stream listing did not finish: {not_finished}"))
            })?;
    for stream_graph in stream_graphs {
        give_the_stream_graph_its_listed_condition(stream_graph, &listings);
    }
    Ok(())
}

/// Write into `stream_graph` the condition `listings` give the stream it names; a stream the
/// listing no longer holds keeps none.
pub(crate) fn give_the_stream_graph_its_listed_condition(
    stream_graph: &mut Value,
    listings: &[StreamListing],
) {
    let Some(stream_graph) = stream_graph.as_object_mut() else {
        return;
    };
    let Some(listing) = stream_graph
        .get("stream")
        .and_then(Value::as_str)
        .and_then(|stream_id| {
            listings
                .iter()
                .find(|listing| listing.stream_id == stream_id)
        })
    else {
        return;
    };
    match serde_json::to_value(&listing.condition) {
        Ok(condition) => {
            stream_graph.insert("condition".to_owned(), condition);
        }
        Err(not_serialized) => tracing::warn!(
            "the stream `{}`'s condition was left out of its graph: {not_serialized}",
            listing.stream_id
        ),
    }
}

/// The native node types every stream can add, then each loaded stream's
/// types described in its own interpreter: what `/api/registry` and the
/// `streamlib://node-catalog` resource answer when no stream is named. A
/// stream unloaded while this renders is left out.
pub(crate) fn machine_wide_registry_response(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> MachineWideRegistryResponse {
    MachineWideRegistryResponse {
        nodes: node_catalog_response(&NODE_REGISTRY.list_registered()).nodes,
        families: PortFamilyDescriptorOutput::every_port_family(),
        streams: operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .into_iter()
            .filter_map(|stream_id| {
                let described = operations_on_the_loaded_streams
                    .node_types_described_in_the_interpreter_of_the_stream_a_call_names(&stream_id)
                    .ok()?;
                let installed = operations_on_the_loaded_streams
                    .node_types_installed_modules_declare_in_the_stream_a_call_names(&stream_id)
                    .ok()?;
                Some(NodeTypesDescribedInOneStreamOutput {
                    stream: stream_id,
                    nodes: node_catalog_response(&described).nodes,
                    installed: installed
                        .iter()
                        .map(|(descriptor, installed_module)| {
                            NodeTypeAnInstalledModuleDeclaresOutput {
                                distribution: installed_module.distribution.clone(),
                                distribution_version: installed_module.distribution_version.clone(),
                                entry_point: installed_module.entry_point.clone(),
                                node: NodeDescriptorOutput::from(descriptor),
                            }
                        })
                        .collect(),
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
        (status = 200, description = "With `stream`, that stream's current graph; without, `{machine, streams}` holding every loaded stream's. Each is an object the control contract's `graph` answer types: `StreamGraphOnTheWire` and `MachineWideGraphOnTheWire`", body = Object),
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
    match the_stream_graph_with_its_condition(&state, stream_operations.as_ref()).await {
        Ok(graph) => Json(graph).into_response(),
        Err(export_failure) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &export_failure),
    }
}

/// The graph `stream_operations` exports, with its stream's condition.
async fn the_stream_graph_with_its_condition(
    state: &AppState,
    stream_operations: &dyn RuntimeOperations,
) -> Result<Value> {
    let mut graph = stream_operations.the_graph_as_answered_async().await?;
    give_the_stream_graphs_their_condition(
        &state.operations_on_the_loaded_streams,
        std::slice::from_mut(&mut graph),
    )
    .await?;
    Ok(graph)
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    params(
        ("stream" = Option<String>, Query, description = "The stream whose node catalog to list, by its stream id, loaded or listed and not loaded; absent lists the native types and each loaded stream's own")
    ),
    responses(
        (status = 200, description = "With `stream`, the node types that stream can add: the natively compiled ones, the views its project brings, then, for a loaded stream, the ones described in its own interpreter; a stream listed and not loaded reads none of its node code. Without, `{nodes, streams}`: the native types, then each loaded stream's described types under that stream", body = NodeTypesOfOneStreamOrOfEveryStream),
        (status = 404, description = "This runtime holds no stream of that id; the error names the loaded and the recorded streams", body = ErrorResponse)
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
    // Off the async runtime: a stream not loaded is held from its graph document, which waits
    // for any stream action under way.
    let operations_on_the_loaded_streams = Arc::clone(&state.operations_on_the_loaded_streams);
    let node_catalog = tokio::task::spawn_blocking(move || {
        operations_on_the_loaded_streams.node_catalog_of_the_stream_a_call_names(&stream)
    })
    .await;
    match node_catalog {
        Ok(Ok(node_catalog)) => Json(NodeTypesOfOneStreamOrOfEveryStream::OneStream(
            node_catalog_response(&node_catalog),
        ))
        .into_response(),
        Ok(Err(refusal)) => stream_resolution_refusal_response(&refusal),
        Err(not_finished) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the node catalog read did not finish: {not_finished}"),
        ),
    }
}

/// One stream's node catalog, rendered as `/api/registry` and the MCP node
/// catalog resource both serve it.
pub(crate) fn node_catalog_response(node_catalog: &[NodeDescriptor]) -> RegistryResponse {
    RegistryResponse {
        nodes: node_catalog
            .iter()
            .map(NodeDescriptorOutput::from)
            .collect(),
        families: PortFamilyDescriptorOutput::every_port_family(),
    }
}

pub(crate) async fn get_openapi_spec(
    State(state): State<AppState>,
) -> Json<utoipa::openapi::OpenApi> {
    Json(state.openapi)
}

#[cfg(test)]
pub(crate) mod router_surface_tests;

#[cfg(test)]
mod control_plane_request_trace_level_tests;
