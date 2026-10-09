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

/// The `stream` query parameter a route reading one stream takes.
#[derive(Deserialize)]
pub(crate) struct StreamSelectionQuery {
    /// The loaded stream the call names; absent names the only loaded one.
    pub stream: Option<String>,
}

// ============================================================================
// Request/Response Types with OpenAPI Schema
// ============================================================================

/// Body of `POST /api/runtime/shutdown`: ask every stream the runtime loads to
/// shut down.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct RuntimeShutdownRequest {
    /// Human-readable attribution logged with the request. Omit for
    /// unspecified.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Wire-visible status token every surface answers an accepted shutdown request
/// with — the REST `202` body and the MCP `shutdown` tool result alike.
pub(crate) const RUNTIME_SHUTDOWN_REQUESTED_STATUS: &str = "RuntimeShutdownRequested";

/// Body returned alongside `202 Accepted` by `POST /api/runtime/shutdown`; the
/// request was handed to the machine's shutdown request and teardown is not
/// awaited.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeShutdownAcceptedResponse {
    /// Typed discriminator: always [`RUNTIME_SHUTDOWN_REQUESTED_STATUS`].
    pub status: &'static str,
    /// The attribution recorded with the request (empty when unspecified).
    pub reason: String,
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
        description = "Observation API for a running StreamLib node: the graph of each stream it loads, each stream's node catalog, its channels, and its event stream. A route reading one stream takes an optional `stream` query parameter naming it; absent names the only loaded stream. A node's graph is defined by its code, so this API does not mutate it.",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "runtime", description = "Runtime lifecycle control: the shutdown of every loaded stream"),
        (name = "surfaces", description = "Published-surface pixel exchange"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
