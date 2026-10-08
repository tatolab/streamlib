// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP resources a node serves beside its tools: its node catalog and its
//! live graph, each rendered at the moment it is read.
//!
//! Both are read-only and expose nothing the control plane does not already
//! serve — the catalog is `/api/registry`'s document, the graph is the `graph`
//! tool's.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::{Value, json};
use streamlib::sdk::runtime::RuntimeOperations;

use crate::handlers::processor_catalog_of_this_process;

/// Every node type the node can add, with its config schema and ports.
pub(crate) const NODE_CATALOG_RESOURCE_URI: &str = "streamlib://node-catalog";

/// The node's running graph: nodes, links, states, metrics.
pub(crate) const LIVE_GRAPH_RESOURCE_URI: &str = "streamlib://graph";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The `resources/list` result.
pub(crate) fn resources_list_result() -> ListResourcesResult {
    ListResourcesResult::with_all_items(vec![
        Resource::new(NODE_CATALOG_RESOURCE_URI, "node-catalog")
            .with_title("Node catalog")
            .with_description("Every node type this node can add, each under the `type` `add_node` takes: its description, its config schema (JSON Schema 2020-12; the keys `add_node`'s `config` takes) and its input and output ports. A Python class appears once the stream's own interpreter has described it.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("The node's running graph as the `graph` tool returns it: nodes by name with their types, ports, config and state, links with their state, and the ports it exposes.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
    ])
}

/// Answer `resources/read`, rendering the resource at `uri` now.
pub(crate) async fn read_resource(
    runtime: &Arc<dyn RuntimeOperations>,
    uri: &str,
) -> Result<ReadResourceResult, McpError> {
    let rendering = match uri {
        NODE_CATALOG_RESOURCE_URI => {
            serde_json::to_string_pretty(&processor_catalog_of_this_process())
        }
        LIVE_GRAPH_RESOURCE_URI => {
            serde_json::to_string_pretty(&exported_live_graph_json(runtime).await?)
        }
        other => {
            return Err(McpError::resource_not_found(
                format!(
                    "no resource at `{other}`; `resources/list` names the ones this node serves"
                ),
                Some(json!({ "uri": other })),
            ));
        }
    };
    let text = rendering.map_err(|e| {
        McpError::internal_error(format!("resource `{uri}` rendering failed: {e}"), None)
    })?;

    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type(JSON_RESOURCE_MIME_TYPE),
    ]))
}

/// The runtime's graph export — the document the `graph` resource serves and
/// the prompts render against.
pub(crate) async fn exported_live_graph_json(
    runtime: &Arc<dyn RuntimeOperations>,
) -> Result<Value, McpError> {
    runtime
        .to_json_async()
        .await
        .map_err(|e| McpError::internal_error(format!("graph export failed: {e}"), None))
}
