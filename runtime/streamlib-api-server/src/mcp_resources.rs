// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP resources the runtime serves beside its tools: its node catalog and
//! its live graph, each rendered at the moment it is read.
//!
//! Both are read-only and expose nothing the control plane does not already
//! serve — the catalog is `/api/registry`'s document without a stream, the
//! graph is the `graph` tool's without one: every loaded stream.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::{Value, json};
use streamlib::sdk::error::Error;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations};

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
            .with_description("Every node type `add_node` can take, each under the `type` it takes: its description, its config schema (JSON Schema 2020-12; the keys `add_node`'s `config` takes) and its input and output ports. `nodes` holds the types compiled into the runtime, which any stream can add; `streams` holds, under each loaded stream's name, the Python types its own interpreter has described, which only that stream can add.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("Every loaded stream's running graph as the `graph` tool returns it without `stream`: `runtime_name`, and under `streams` each stream's nodes by name with their types, ports, config and state, its links with their state, and the ports it exposes.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
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

/// The operations on the loaded stream a prompt names, or the refusal — naming
/// the loaded streams — as invalid params.
pub(crate) fn the_stream_a_prompt_names(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    stream_name: &str,
) -> Result<Arc<dyn RuntimeOperations>, McpError> {
    operations_on_the_loaded_streams
        .runtime_operations_of_the_stream_a_call_names(stream_name)
        .map_err(|refusal| prompt_stream_refusal(&refusal))
}

/// A refused stream lookup as a prompt's invalid params; any other failure as
/// an internal error.
pub(crate) fn prompt_stream_refusal(refusal: &Error) -> McpError {
    match refusal {
        Error::NotFound(_) => McpError::invalid_params(refusal.to_string(), None),
        other => McpError::internal_error(other.to_string(), None),
    }
}

/// One stream's graph export — the document the prompts render against.
pub(crate) async fn exported_live_graph_json(
    stream_operations: &Arc<dyn RuntimeOperations>,
) -> Result<Value, McpError> {
    stream_operations
        .to_json_async()
        .await
        .map_err(|e| McpError::internal_error(format!("graph export failed: {e}"), None))
}
