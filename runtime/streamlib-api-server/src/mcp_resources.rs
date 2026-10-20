// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP resources the runtime serves beside its tools: its node catalog, its
//! live graph and its relay-adapter catalog, each rendered at the moment it is
//! read.
//!
//! Each is read-only and exposes nothing the control plane does not already
//! serve — the node catalog is `/api/registry`'s document without a stream,
//! the graph is the `graph` tool's without one, the relay-adapter catalog is
//! `/api/relay-adapters`'s document.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::json;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine;

use crate::handlers::{machine_wide_graph_json, machine_wide_registry_response};
use crate::state::RelayAdapterCatalogResponse;

/// Every node type the runtime's streams can add, with its config schema and
/// ports.
pub(crate) const NODE_CATALOG_RESOURCE_URI: &str = "streamlib://node-catalog";

/// Every loaded stream's running graph: nodes, links, states, metrics.
pub(crate) const LIVE_GRAPH_RESOURCE_URI: &str = "streamlib://graph";

/// Every relay adapter `add_relay` takes, with its settings schema.
pub(crate) const RELAY_ADAPTER_CATALOG_RESOURCE_URI: &str = "streamlib://relay-adapter-catalog";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The `resources/list` result.
pub(crate) fn resources_list_result() -> ListResourcesResult {
    ListResourcesResult::with_all_items(vec![
        Resource::new(NODE_CATALOG_RESOURCE_URI, "node-catalog")
            .with_title("Node catalog")
            .with_description("Every node type `add_node` can take, each under the `type` it takes: its description, its config schema (JSON Schema 2020-12; the keys `add_node`'s `config` takes) and its input and output ports. `nodes` holds the types compiled into the runtime, which any stream can add; `streams` holds, under each loaded stream's name, the Python types its own interpreter has described, which only that stream can add.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("Every loaded stream's running graph as the `graph` tool returns it without `stream`: `machine`, this machine's name, and under `streams` each stream's nodes by name with their types, ports, config and state, its links with their state, and the ports it exposes.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(RELAY_ADAPTER_CATALOG_RESOURCE_URI, "relay-adapter-catalog")
            .with_title("Relay-adapter catalog")
            .with_description("Every relay adapter compiled into the runtime, under `adapters`: its `kind`, as `add_relay` takes it, what it joins (`doc`), and its `settings_schema` (JSON Schema 2020-12; the keys `add_relay` and `configure_relay` take in `settings`), each secret marked `writeOnly` — kept in the OS secret store and listed by `list_relays` only as set or unset.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
    ])
}

/// Answer `resources/read`, rendering the resource at `uri` now.
pub(crate) async fn read_resource(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    relay_instances: &dyn RelayInstancesOfThisMachine,
    uri: &str,
) -> Result<ReadResourceResult, McpError> {
    let rendering = match uri {
        NODE_CATALOG_RESOURCE_URI => serde_json::to_string_pretty(&machine_wide_registry_response(
            operations_on_the_loaded_streams,
        )),
        LIVE_GRAPH_RESOURCE_URI => serde_json::to_string_pretty(
            &machine_wide_graph_json(operations_on_the_loaded_streams)
                .await
                .map_err(|e| McpError::internal_error(format!("graph export failed: {e}"), None))?,
        ),
        RELAY_ADAPTER_CATALOG_RESOURCE_URI => {
            serde_json::to_string_pretty(&RelayAdapterCatalogResponse::of(relay_instances))
        }
        other => {
            return Err(McpError::resource_not_found(
                format!(
                    "no resource at `{other}`; `resources/list` names the ones this runtime serves"
                ),
                Some(json!({ "uri": other })),
            ));
        }
    };
    json_resource_contents(uri, rendering)
}

fn json_resource_contents(
    uri: &str,
    rendering: serde_json::Result<String>,
) -> Result<ReadResourceResult, McpError> {
    let text = rendering.map_err(|e| {
        McpError::internal_error(format!("resource `{uri}` rendering failed: {e}"), None)
    })?;

    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type(JSON_RESOURCE_MIME_TYPE),
    ]))
}
