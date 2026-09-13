// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP resources a node serves beside its tools: its processor catalog and
//! its live graph, each rendered at the moment it is read.
//!
//! Both are read-only and expose nothing the control plane does not already
//! serve — the catalog is `/api/registry`'s document, the graph is the `graph`
//! tool's — and both sit behind the same `POST /mcp` gate the tools do.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use streamlib::sdk::runtime::RuntimeOperations;

use crate::handlers::processor_catalog_of_this_process;
use crate::mcp::RpcError;

/// Every processor type the node can add, with its config schema and ports.
pub(crate) const PROCESSOR_CATALOG_RESOURCE_URI: &str = "streamlib://processor-catalog";

/// The node's running graph: processors, links, states, metrics, extensions.
pub(crate) const LIVE_GRAPH_RESOURCE_URI: &str = "streamlib://graph";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The `resources/list` result.
pub(crate) fn resources_list_result() -> Value {
    json!({
        "resources": [
            {
                "uri": PROCESSOR_CATALOG_RESOURCE_URI,
                "name": "processor-catalog",
                "title": "Processor catalog",
                "description": "Every processor type this node can add, by the import path `add_processor` takes: its description, its config schema (JSON Schema 2020-12; the keys `add_processor`'s `config` takes) and its input and output ports. A Python class appears once the app has imported its module.",
                "mimeType": JSON_RESOURCE_MIME_TYPE,
            },
            {
                "uri": LIVE_GRAPH_RESOURCE_URI,
                "name": "graph",
                "title": "Live graph",
                "description": "The node's running graph as the `graph` tool returns it: processors with their ids, ports, config and state, links with their state, and the capability extensions loaded.",
                "mimeType": JSON_RESOURCE_MIME_TYPE,
            },
        ]
    })
}

/// The `resources/templates/list` result: the node serves no parameterised
/// resource, and says so rather than refusing the method a client asks for as
/// soon as it sees the resources capability.
pub(crate) fn resource_templates_list_result() -> Value {
    json!({ "resourceTemplates": [] })
}

/// Answer `resources/read`, rendering the named resource now.
pub(crate) async fn read_resource(
    runtime: &Arc<dyn RuntimeOperations>,
    params: Value,
) -> std::result::Result<Value, RpcError> {
    #[derive(Deserialize)]
    struct ReadResourceParams {
        uri: String,
    }
    let ReadResourceParams { uri } = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("malformed resources/read params: {e}")))?;

    let document = match uri.as_str() {
        PROCESSOR_CATALOG_RESOURCE_URI => serde_json::to_value(processor_catalog_of_this_process())
            .map_err(|e| RpcError::internal(format!("processor catalog rendering failed: {e}")))?,
        LIVE_GRAPH_RESOURCE_URI => runtime
            .to_json_async()
            .await
            .map_err(|e| RpcError::internal(format!("graph export failed: {e}")))?,
        other => return Err(RpcError::resource_not_found(other)),
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|e| RpcError::internal(format!("resource `{uri}` rendering failed: {e}")))?;

    Ok(json!({
        "contents": [{ "uri": uri, "mimeType": JSON_RESOURCE_MIME_TYPE, "text": text }]
    }))
}
