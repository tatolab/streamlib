// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The MCP resources the runtime serves beside its tools: its node catalog and
//! its live graph, each rendered at the moment it is read.
//!
//! Each is read-only and exposes nothing the control plane does not already
//! serve — the node catalog is `/api/registry`'s document without a stream,
//! the graph is the `graph` tool's without one.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::json;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;

use crate::handlers::{machine_wide_graph_json, machine_wide_registry_response};

/// Every node type the runtime's streams can add, with its config schema and
/// ports.
pub(crate) const NODE_CATALOG_RESOURCE_URI: &str = "streamlib://node-catalog";

/// Every loaded stream's running graph: nodes, links, states, metrics.
pub(crate) const LIVE_GRAPH_RESOURCE_URI: &str = "streamlib://graph";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The `resources/list` result.
pub(crate) fn resources_list_result() -> ListResourcesResult {
    ListResourcesResult::with_all_items(vec![
        Resource::new(NODE_CATALOG_RESOURCE_URI, "node-catalog")
            .with_title("Node catalog")
            .with_description("Every node type `add_node` can take, each under the `type` it takes: its description, its config schema (JSON Schema 2020-12; the keys `add_node`'s `config` takes) and its input and output ports. `nodes` holds the types compiled into the runtime, which any stream can add; `streams` holds, under each loaded stream's name, the Python types its own interpreter has described, which only that stream can add: under `nodes` the ones its graph or a live add named, and under `installed` the ones installed distributions declare in its environment's `tatolab.nodes` entry points, each with its `distribution`, `distribution_version` and `entry_point`.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("Every loaded stream's running graph as the `graph` tool returns it without `stream`: `machine`, this machine's name, and under `streams` each stream's nodes by name with their types, ports, config and state, and its links with their state.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        crate::mcp_stream_action_resources::stream_actions_under_way_resource(),
    ])
}

/// Answer `resources/read`, rendering the resource at `uri` now.
pub(crate) async fn read_resource(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
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
