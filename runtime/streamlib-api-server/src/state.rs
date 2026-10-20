// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_relay_adapter_sdk::{RelayAdapterCatalogEntry, RelayInstancesOfThisMachine};
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    pub relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
    pub openapi: utoipa::openapi::OpenApi,
}

/// The relay-adapter catalog: every adapter compiled into the runtime.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RelayAdapterCatalogResponse {
    /// Each adapter, as `add_relay` takes its kind.
    pub adapters: Vec<RelayAdapterCatalogResponseAdapter>,
}

/// One adapter of a [`RelayAdapterCatalogResponse`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RelayAdapterCatalogResponseAdapter {
    /// The adapter's kind, as `add_relay` takes it.
    pub kind: String,
    /// What the adapter joins.
    pub doc: String,
    /// The JSON Schema of an instance's settings; each secret is `writeOnly`.
    #[schema(value_type = Object)]
    pub settings_schema: serde_json::Value,
}

impl RelayAdapterCatalogResponse {
    /// The catalog `relay_instances` serves.
    pub(crate) fn of(relay_instances: &dyn RelayInstancesOfThisMachine) -> Self {
        Self {
            adapters: relay_instances
                .relay_adapter_catalog()
                .into_iter()
                .map(
                    |RelayAdapterCatalogEntry {
                         kind,
                         doc,
                         settings_schema,
                     }| RelayAdapterCatalogResponseAdapter {
                        kind,
                        doc,
                        settings_schema,
                    },
                )
                .collect(),
        }
    }
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
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Observation API for the machine's runtime: the graph of each stream it loads, the node types each can add, and its event stream. `/api/relay-adapters` lists the relay adapters a relay instance is added through, each with its settings schema. `/api/graph` and `/api/registry` take an optional `stream` query parameter naming one loaded stream, and read every loaded stream without it. A stream's graph is defined by its code, so this API does not mutate it.",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "relays", description = "The relay adapters compiled into the runtime"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
