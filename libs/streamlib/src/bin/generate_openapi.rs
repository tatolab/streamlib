// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(dead_code)] // Structs/functions are only used for schema generation

//! Generates OpenAPI specification for the StreamLib Runtime API.
//!
//! Run with: `cargo run --bin generate_openapi`
//!
//! This generates an OpenAPI 3.0 spec in `dist/schemas/openapi.json` that can be used for:
//! - API documentation
//! - TypeScript client generation
//! - API testing tools

use std::fs;
use std::path::Path;
use utoipa::OpenApi;

// Re-create the OpenAPI doc structure from api_server.rs
// We need to duplicate this because the original is private to the processor module

use serde::{Deserialize, Serialize};
use streamlib::core::json_schema::{GraphResponse, RegistryResponse};

#[derive(Deserialize, utoipa::ToSchema)]
struct CreateProcessorRequest {
    /// The processor type name (e.g., "CameraProcessor", "DisplayProcessor")
    processor_type: String,
    /// Processor-specific configuration as JSON
    config: serde_json::Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct CreateConnectionRequest {
    /// Source processor ID
    from_processor: String,
    /// Source output port name
    from_port: String,
    /// Destination processor ID
    to_processor: String,
    /// Destination input port name
    to_port: String,
}

#[derive(Serialize, utoipa::ToSchema)]
struct IdResponse {
    /// The created resource ID
    id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ErrorResponse {
    /// Error message
    error: String,
}

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
        (name = "events", description = "Real-time event streaming via WebSocket")
    ),
    paths(
        health,
        get_graph,
        create_processor,
        delete_processor,
        create_connection,
        delete_connection,
        get_registry
    ),
    components(
        schemas(
            CreateProcessorRequest,
            CreateConnectionRequest,
            IdResponse,
            ErrorResponse,
            RegistryResponse,
            GraphResponse
        )
    )
)]
struct ApiDoc;

// Dummy path handlers for OpenAPI generation (these just define the schema)

#[utoipa::path(
    get,
    path = "/health",
    tag = "graph",
    responses(
        (status = 200, description = "Server is healthy", body = String)
    )
)]
fn health() {}

#[utoipa::path(
    get,
    path = "/api/graph",
    tag = "graph",
    responses(
        (status = 200, description = "Current graph state", body = GraphResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
fn get_graph() {}

#[utoipa::path(
    post,
    path = "/api/processor",
    tag = "processors",
    request_body = CreateProcessorRequest,
    responses(
        (status = 200, description = "Processor created successfully", body = IdResponse),
        (status = 400, description = "Invalid processor type or configuration", body = ErrorResponse)
    )
)]
fn create_processor() {}

#[utoipa::path(
    delete,
    path = "/api/processors/{id}",
    tag = "processors",
    params(
        ("id" = String, Path, description = "Processor ID to delete")
    ),
    responses(
        (status = 204, description = "Processor deleted successfully"),
        (status = 404, description = "Processor not found", body = ErrorResponse)
    )
)]
fn delete_processor() {}

#[utoipa::path(
    post,
    path = "/api/connections",
    tag = "connections",
    request_body = CreateConnectionRequest,
    responses(
        (status = 200, description = "Connection created successfully", body = IdResponse),
        (status = 400, description = "Invalid connection", body = ErrorResponse)
    )
)]
fn create_connection() {}

#[utoipa::path(
    delete,
    path = "/api/connections/{id}",
    tag = "connections",
    params(
        ("id" = String, Path, description = "Connection ID to delete")
    ),
    responses(
        (status = 204, description = "Connection deleted successfully"),
        (status = 404, description = "Connection not found", body = ErrorResponse)
    )
)]
fn delete_connection() {}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    responses(
        (status = 200, description = "Available processors and schemas", body = RegistryResponse)
    )
)]
fn get_registry() {}

fn main() {
    let schema_dir = Path::new("dist/schemas");

    // Create the schema directory if it doesn't exist
    if !schema_dir.exists() {
        fs::create_dir_all(schema_dir).expect("Failed to create schema directory");
        println!("Created directory: {}", schema_dir.display());
    }

    // Generate OpenAPI spec
    let openapi = ApiDoc::openapi();
    let openapi_json =
        serde_json::to_string_pretty(&openapi).expect("Failed to serialize OpenAPI spec");
    let openapi_path = schema_dir.join("openapi.json");
    fs::write(&openapi_path, &openapi_json).expect("Failed to write OpenAPI spec");
    println!("Generated: {}", openapi_path.display());

    println!("\nOpenAPI generation complete!");
}
