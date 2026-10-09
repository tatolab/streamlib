// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MCP resources a node serves beside its tools: its node catalog and its
//! live graph, each rendered at the moment it is read.
//!
//! Both are read-only and expose nothing the control plane does not already
//! serve — the catalog is `/api/registry`'s document, the graph is the `graph`
//! tool's. Each renders the sole loaded stream; with none or several loaded it
//! says which streams are.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{ListResourcesResult, ReadResourceResult, Resource, ResourceContents};
use serde_json::{Value, json};
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::Error;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations};

use crate::handlers::node_catalog_response;

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
            .with_description("Every node type the sole loaded stream can add, each under the `type` `add_node` takes: its description, its config schema (JSON Schema 2020-12; the keys `add_node`'s `config` takes) and its input and output ports. A Python class appears once the stream's own interpreter has described it. With several streams loaded it names them instead.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
        Resource::new(LIVE_GRAPH_RESOURCE_URI, "graph")
            .with_title("Live graph")
            .with_description("The sole loaded stream's running graph as the `graph` tool returns it: nodes by name with their types, ports, config and state, links with their state, and the ports it exposes. With several streams loaded it names them instead.")
            .with_mime_type(JSON_RESOURCE_MIME_TYPE),
    ])
}

/// What a lookup of the stream a render names found: that stream's part, or —
/// when the name meets no loaded stream, or `None` meets none or several — the
/// names of the streams loaded now.
pub(crate) enum TheStreamsPartOrTheLoadedStreams<StreamsPart> {
    /// The part of the stream the render names.
    TheStreamsPart(StreamsPart),
    /// The cast names of the streams loaded when the lookup was refused.
    TheLoadedStreams(Vec<String>),
}

/// Turn a refused stream lookup into the names of the loaded streams; any
/// other failure stays an error.
fn the_streams_part_or_the_loaded_streams<StreamsPart>(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    stream_lookup: streamlib::sdk::error::Result<StreamsPart>,
) -> Result<TheStreamsPartOrTheLoadedStreams<StreamsPart>, McpError> {
    match stream_lookup {
        Ok(streams_part) => Ok(TheStreamsPartOrTheLoadedStreams::TheStreamsPart(
            streams_part,
        )),
        Err(Error::NotFound(_)) => Ok(TheStreamsPartOrTheLoadedStreams::TheLoadedStreams(
            operations_on_the_loaded_streams.names_of_the_loaded_streams(),
        )),
        Err(other) => Err(McpError::internal_error(other.to_string(), None)),
    }
}

/// The sole loaded stream's operations, resolved by the engine's own rule
/// for a call that names no stream, or the names of the loaded streams.
pub(crate) fn the_sole_loaded_streams_operations_or_the_loaded_streams(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Result<TheStreamsPartOrTheLoadedStreams<Arc<dyn RuntimeOperations>>, McpError> {
    the_streams_part_or_the_loaded_streams(
        operations_on_the_loaded_streams,
        operations_on_the_loaded_streams.runtime_operations_of_the_stream_a_call_names(None),
    )
}

/// The node catalog of the stream `stream_name` names — `None` names the sole
/// loaded stream — or the names of the loaded streams.
pub(crate) fn the_node_catalog_of_the_stream_or_the_loaded_streams(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    stream_name: Option<&str>,
) -> Result<TheStreamsPartOrTheLoadedStreams<Vec<ProcessorDescriptor>>, McpError> {
    the_streams_part_or_the_loaded_streams(
        operations_on_the_loaded_streams,
        operations_on_the_loaded_streams.node_catalog_of_the_stream_a_call_names(stream_name),
    )
}

/// What a resource or prompt says in place of rendering the sole loaded
/// stream, given the streams loaded when its lookup was refused.
pub(crate) fn which_streams_are_loaded_in_place_of_the_sole_one(
    loaded_stream_names: &[String],
) -> String {
    match loaded_stream_names {
        [] => "No stream is loaded in this runtime, so there is no stream to render.".to_string(),
        [only] => format!(
            "The streams loaded in this runtime changed while this rendered; only `{only}` is \
             loaded now. Read this again."
        ),
        several => format!(
            "{} streams are loaded in this runtime: {}. This renders the sole loaded stream only; \
             call `graph` with `stream` naming one of them, and name the same `stream` in every \
             tool call about it.",
            several.len(),
            several.join(", ")
        ),
    }
}

/// The JSON a resource serves in place of rendering the sole loaded stream.
fn which_streams_are_loaded_json(loaded_stream_names: &[String]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&json!({
        "loaded_streams": loaded_stream_names,
        "note": which_streams_are_loaded_in_place_of_the_sole_one(loaded_stream_names),
    }))
}

/// Answer `resources/read`, rendering the resource at `uri` now.
pub(crate) async fn read_resource(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    uri: &str,
) -> Result<ReadResourceResult, McpError> {
    let rendering = match uri {
        NODE_CATALOG_RESOURCE_URI => match the_node_catalog_of_the_stream_or_the_loaded_streams(
            operations_on_the_loaded_streams,
            None,
        )? {
            TheStreamsPartOrTheLoadedStreams::TheStreamsPart(node_catalog) => {
                serde_json::to_string_pretty(&node_catalog_response(&node_catalog))
            }
            TheStreamsPartOrTheLoadedStreams::TheLoadedStreams(loaded_stream_names) => {
                which_streams_are_loaded_json(&loaded_stream_names)
            }
        },
        LIVE_GRAPH_RESOURCE_URI => match the_sole_loaded_streams_operations_or_the_loaded_streams(
            operations_on_the_loaded_streams,
        )? {
            TheStreamsPartOrTheLoadedStreams::TheStreamsPart(stream_operations) => {
                serde_json::to_string_pretty(&exported_live_graph_json(&stream_operations).await?)
            }
            TheStreamsPartOrTheLoadedStreams::TheLoadedStreams(loaded_stream_names) => {
                which_streams_are_loaded_json(&loaded_stream_names)
            }
        },
        other => {
            return Err(McpError::resource_not_found(
                format!(
                    "no resource at `{other}`; `resources/list` names the ones this node serves"
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

/// One stream's graph export — the document the `graph` resource serves and
/// the prompts render against.
pub(crate) async fn exported_live_graph_json(
    stream_operations: &Arc<dyn RuntimeOperations>,
) -> Result<Value, McpError> {
    stream_operations
        .to_json_async()
        .await
        .map_err(|e| McpError::internal_error(format!("graph export failed: {e}"), None))
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::runtime::Runner;

    use super::*;

    /// Mental-revert: without the `[only]` arm, a list read back with one name
    /// renders "1 streams are loaded … renders the sole loaded stream only".
    #[test]
    fn the_which_streams_text_fits_every_count_of_loaded_streams() {
        let none = which_streams_are_loaded_in_place_of_the_sole_one(&[]);
        assert!(none.starts_with("No stream is loaded"), "{none}");

        let only = which_streams_are_loaded_in_place_of_the_sole_one(&["first".to_string()]);
        assert!(
            only.contains("only `first` is loaded now") && !only.contains("1 streams"),
            "{only}"
        );

        let several = which_streams_are_loaded_in_place_of_the_sole_one(&[
            "first".to_string(),
            "second".to_string(),
        ]);
        assert!(
            several.starts_with("2 streams are loaded in this runtime: first, second."),
            "{several}"
        );
    }

    /// Mental-revert: mapping every error to the loaded streams answers a
    /// failure that is not a refused lookup as "which streams are loaded".
    #[test]
    fn only_a_refused_stream_lookup_becomes_the_loaded_streams() {
        let engine: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> =
            Runner::new().expect("the engine builds");

        match the_streams_part_or_the_loaded_streams(&engine, Ok(7)) {
            Ok(TheStreamsPartOrTheLoadedStreams::TheStreamsPart(7)) => {}
            _ => panic!("a found part passes through"),
        }
        match the_streams_part_or_the_loaded_streams::<()>(
            &engine,
            Err(Error::NotFound("no stream".to_string())),
        ) {
            Ok(TheStreamsPartOrTheLoadedStreams::TheLoadedStreams(loaded_stream_names)) => {
                assert!(loaded_stream_names.is_empty(), "{loaded_stream_names:?}")
            }
            _ => panic!("a refused lookup becomes the loaded streams"),
        }
        match the_streams_part_or_the_loaded_streams::<()>(
            &engine,
            Err(Error::Runtime("the export broke".to_string())),
        ) {
            Err(mcp_error) => assert!(mcp_error.message.contains("the export broke")),
            Ok(_) => panic!("a failure that is not a refused lookup stays an error"),
        }
    }
}
