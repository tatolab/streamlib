// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::json_schema::SchemaIdentOutput;
use streamlib::sdk::runtime::RuntimeOperations;
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub runtime: Arc<dyn RuntimeOperations>,
    /// Runtime id — used to look up the matching MoQ session registry
    /// inside `@tatolab/moq` when the `moq` feature is enabled.
    #[cfg(feature = "moq")]
    pub runtime_id: String,
    pub openapi: utoipa::openapi::OpenApi,
}

// ============================================================================
// Request/Response Types with OpenAPI Schema
// ============================================================================

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CreateProcessorRequest {
    /// Structured processor identity — the four-field map form of
    /// `@org/package/Type@version`. The structured-everywhere rule
    /// applies on the HTTP API too — bare strings like
    /// `"CameraProcessor"` are rejected at deserialize time.
    pub processor_type: SchemaIdentOutput,
    /// Processor-specific configuration as JSON
    pub config: serde_json::Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CreateConnectionRequest {
    /// Source processor ID
    pub from_processor: String,
    /// Source output port name
    pub from_port: String,
    /// Destination processor ID
    pub to_processor: String,
    /// Destination input port name
    pub to_port: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct IdResponse {
    /// The created resource ID
    pub id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
    /// Error message
    pub error: String,
}

/// Body returned alongside `422 Unprocessable Entity` when the caller
/// supplies a structurally-valid `SchemaIdent` whose type isn't registered.
/// The runtime is dynamic — types load and unload — so this is a normal
/// runtime miss, not a malformed request. The client gets a typed
/// discriminator (`error`), the offending ident, and the placeholder
/// processor id (the failed node is left in the graph in `Error` state for
/// observability via `GET /api/graph`).
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct UnknownProcessorTypeResponse {
    /// Typed error discriminator: always `"UnknownProcessorType"`.
    pub error: &'static str,
    /// The structured ident that didn't resolve.
    pub ident: SchemaIdentOutput,
}

/// Body returned alongside `404 Not Found` when a connection references a
/// processor id that doesn't exist in the graph.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ProcessorNotFoundResponse {
    /// Typed error discriminator: always `"ProcessorNotFound"`.
    pub error: &'static str,
    /// The processor id that wasn't in the graph.
    pub processor_id: String,
}

/// Body returned alongside `422 Unprocessable Entity` when a connection
/// references a port name that doesn't exist on the named processor.
/// Distinct from `UnknownProcessorTypeResponse`: the processor exists,
/// but the port doesn't.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ProcessorPortNotFoundResponse {
    /// Typed error discriminator: always `"ProcessorPortNotFound"`.
    pub error: &'static str,
    /// The processor id whose port lookup failed.
    pub processor_id: String,
    /// The port name that wasn't found.
    pub port_name: String,
    /// `"input"` or `"output"`.
    pub direction: &'static str,
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Real-time streaming infrastructure API for managing processors, connections, and events",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection and management"),
        (name = "processors", description = "Processor lifecycle management"),
        (name = "connections", description = "Connection management between processors"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "schemas", description = "Schema definitions"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
