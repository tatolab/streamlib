// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A refusal of a port a node does not have suggests the call that describes the ports its node
//! type does have.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};
use streamlib::sdk::runtime::StreamLocation;
use streamlib_control_contract::{ControlOperation, DescribeNodeTypes, the_suggested_call};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::{GraphTransactionRefusal, StreamGraphTransactionExecutor};

impl StreamGraphTransactionExecutor<'_> {
    /// `refusal`, suggesting the ports of the node type it names when it refuses a port; the
    /// stream of the refused operation is the one at its place in `stream_id_of_each_request`.
    pub(super) fn suggesting_the_ports_of_the_node_type_a_port_not_found_names(
        &self,
        refusal: GraphTransactionRefusal,
        stream_id_of_each_request: &[String],
        stream_locations: &HashMap<String, StreamLocation>,
    ) -> GraphTransactionRefusal {
        if refusal.the_node_whose_port_was_not_found().is_none() {
            return refusal;
        }
        let Some((stream_id, stream_location)) = refusal
            .refused_operation_index
            .and_then(|index| stream_id_of_each_request.get(index))
            .and_then(|stream_id| Some((stream_id, stream_locations.get(stream_id)?)))
        else {
            return refusal;
        };
        let rendered_graph = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(stream_id)
            .ok()
            .and_then(|stream_operations| {
                self.async_runtime
                    .block_on(stream_operations.to_json_async())
                    .ok()
            });
        match rendered_graph {
            Some(rendered_graph) => refusal.suggesting_the_ports_of_the_node_type_it_names(
                &stream_location.project_directory,
                &rendered_graph,
            ),
            None => refusal,
        }
    }
}

impl GraphTransactionRefusal {
    /// This refusal, suggesting `describe_node_types` on the type of the node it names, in the
    /// project at `project_directory`, when it refuses a port that node does not have.
    /// `rendered_graph` is the `graph` render of the stream it was refused on; a node it does
    /// not render, one the transaction itself added among them, suggests nothing.
    pub(crate) fn suggesting_the_ports_of_the_node_type_it_names(
        mut self,
        project_directory: &Path,
        rendered_graph: &Value,
    ) -> Self {
        let Some(node_type) = self
            .the_node_whose_port_was_not_found()
            .and_then(|node| the_type_of_the_node_rendered_in(rendered_graph, &node))
        else {
            return self;
        };
        self.suggested_call = the_suggested_call(
            DescribeNodeTypes::METHOD_NAME,
            Map::from_iter([
                ("project_directory".to_owned(), json!(project_directory)),
                ("node_types".to_owned(), json!([node_type])),
            ]),
        )
        .map(Box::new);
        self
    }

    /// The UUID of the node whose port this refusal finds no port of: the node its operation
    /// names, else the end of the link it names whose port it refuses, else the node it names
    /// itself; `None` for any other refusal.
    fn the_node_whose_port_was_not_found(&self) -> Option<String> {
        let code_and_place = &self.refusal.code_and_place;
        if code_and_place.refusal_code != ControlOperationRefusalCode::PortNotFound {
            return None;
        }
        let names = &self.what_the_refused_operation_names;
        let the_links_end = names.link.as_ref().map(|link| {
            if code_and_place.refused_field.as_deref() == Some(link.target.port.as_str()) {
                link.target.node.clone()
            } else {
                link.source.node.clone()
            }
        });
        names
            .node
            .clone()
            .or(the_links_end)
            .or_else(|| code_and_place.refused_node.clone())
    }
}

/// The type of the node `node` that `rendered_level`, a level of a stream's `graph` render,
/// renders, at any level of the fragments it holds.
fn the_type_of_the_node_rendered_in(rendered_level: &Value, node: &str) -> Option<String> {
    rendered_level["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|rendered_node| {
            if rendered_node["id"] == node {
                rendered_node["type"].as_str().map(str::to_owned)
            } else {
                the_type_of_the_node_rendered_in(&rendered_node["fragment"], node)
            }
        })
}
