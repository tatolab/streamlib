// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The local API's router, built by [`build_router`], and the graph rendering the control
//! operations and the MCP resources share.

use axum::{Router, routing::get};
use serde_json::{Value, json};
use std::sync::Arc;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, StreamListing};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_JSON_RPC_ROUTE_PATH, CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
    CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH, MCP_STDIO_UPGRADE_REQUEST_TARGET,
    MCP_STREAMABLE_HTTP_ROUTE_PATH,
};

use crate::loopback_http_listener::ControlWebSocketRouteForTheLoopbackListener;

/// The route a client asks whether the local API answers.
const HEALTH_ROUTE_PATH: &str = "/health";

pub(crate) fn build_router(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    local_api_stopping_token: CancellationToken,
) -> LocalApiRouters {
    let local_api_mcp_server_handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::clone(&operations_on_the_loaded_streams),
        local_api_stopping_token.clone(),
    );
    let local_api_mcp_service =
        crate::mcp::local_api_mcp_streamable_http_service(local_api_mcp_server_handler.clone());
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
    let router = Router::new()
        .route(HEALTH_ROUTE_PATH, get(health))
        .route(
            &crate::control_rest_adapter::control_rest_operation_route_pattern(),
            crate::control_rest_adapter::control_rest_operation_route(
                control_operation_dispatcher.clone(),
            ),
        )
        .route(
            CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH,
            crate::control_rest_adapter::control_rest_openapi_document_route(),
        )
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
        .route_service(MCP_STREAMABLE_HTTP_ROUTE_PATH, local_api_mcp_service)
        .route(
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
            crate::mcp_stdio_upgrade::local_api_mcp_stdio_upgrade_route(
                local_api_mcp_server_handler,
                local_api_stopping_token,
            ),
        );

    LocalApiRouters {
        local_api_socket_router: router.layer(trace_layer),
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

/// `ok`, from a local API that answers.
async fn health() -> &'static str {
    "ok"
}

/// Every loaded stream's graph under the machine's name: what the `graph` operation and the
/// `streamlib://graph` resource answer when no stream is named. A stream unloaded while this
/// renders is left out.
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

#[cfg(test)]
pub(crate) mod router_surface_tests;

#[cfg(test)]
mod control_plane_request_trace_level_tests;
