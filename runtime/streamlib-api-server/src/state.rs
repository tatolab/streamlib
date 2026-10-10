// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    pub openapi: utoipa::openapi::OpenApi,
}

/// The `stream` query parameter of a route that reads one stream or, without
/// it, every loaded stream.
#[derive(Deserialize)]
pub(crate) struct OptionalStreamSelectionQuery {
    /// The loaded stream the call names; absent reads every loaded stream.
    pub stream: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
    /// Error message
    pub error: String,
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(crate::handlers::tap_websocket_handler),
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Observation API for the machine's runtime: the graph of each stream it loads, the node types each can add, its channels, and its event stream. `/api/graph` and `/api/registry` take an optional `stream` query parameter naming one loaded stream, and read every loaded stream without it; a tap names the stream its channel is in. A stream's graph is defined by its code, so this API does not mutate it.",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "surfaces", description = "Published-surface pixel exchange"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
