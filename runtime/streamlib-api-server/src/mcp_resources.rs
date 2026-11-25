// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The MCP resources the runtime serves beside its tools: its live graph and the
//! stream actions under way, each rendered at the moment it is read.
//!
//! Each is read-only. The graph is the `graph` operation's answer without a
//! stream.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::json;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;

use crate::handlers::machine_wide_graph_json;

/// Every loaded stream's running graph: nodes, links, states, metrics.
pub(crate) const LIVE_GRAPH_RESOURCE_URI: &str = "streamlib://graph";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The `resources/list` result.
pub(crate) fn resources_list_result() -> ListResourcesResult {
    ListResourcesResult::with_all_items(vec![
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("Every loaded stream's running graph as the `graph` tool returns it without `stream`: `machine`, this machine's name, and under `streams` each stream's nodes by name with their types, ports, input values and state, and its links with their state.")
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
