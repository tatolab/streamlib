// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::runtime::RuntimeOperations;
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub runtime: Arc<dyn RuntimeOperations>,
    pub openapi: utoipa::openapi::OpenApi,
}

// ============================================================================
// Request/Response Types with OpenAPI Schema
// ============================================================================

/// Body of `POST /api/runtime/shutdown`: ask the runtime to stop.
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
/// request was handed to the runtime's shutdown funnel and teardown is not
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
        description = "Observation API for a running StreamLib node: its processor graph, its registry, its channels, and its event stream. A node's graph is defined by its code, so this API does not mutate it.",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "runtime", description = "Runtime lifecycle control"),
        (name = "surfaces", description = "Published-surface pixel exchange"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
