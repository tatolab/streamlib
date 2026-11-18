// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's input, named by the port its top-level `GraphInput` declares: the node whose
//! `value` input a set naming no node sets.

use uuid::Uuid;

use crate::core::descriptors::PortFamily;
use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PORT_CONFIG_KEY,
};
use crate::core::graph::{
    Graph, GraphNode, GraphNodeWithComponents, NodeInstanceComponent, cast_exposed_name_to_url_safe,
};
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;

/// The top-level `GraphInput` declaring one of a stream's inputs.
#[derive(Debug)]
pub struct TopLevelGraphInputDeclaringAStreamInput {
    /// The `GraphInput` node, by UUID.
    pub node: Uuid,
    /// The family its `value` carries.
    pub family: PortFamily,
    /// Why its node does not run while its stream's playback is not stopped, naming the input;
    /// `None` when it runs, or the playback is stopped.
    pub refusal_while_its_node_does_not_run: Option<Error>,
}

impl LoadedStreamInThisRuntime {
    /// The top-level `GraphInput` of this stream declaring the input `input_name`, compared as
    /// every exposed name is cast. Refused for a name none declares, naming the stream's inputs.
    /// The stream's graph lock is held across a compile, so the read waits off the async workers.
    pub(crate) async fn the_top_level_graph_input_declaring(
        &self,
        input_name: String,
    ) -> Result<TopLevelGraphInputDeclaringAStreamInput> {
        let stream_id = self.stream_id().to_string();
        let why_a_node_that_has_not_failed_does_not_run =
            self.why_a_node_of_this_stream_that_has_not_failed_does_not_run();
        let compiler = std::sync::Arc::clone(&self.compiler);
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                compiler.scope_reading(|graph| {
                    let node = the_top_level_graph_input_declaring(graph, &stream_id, &input_name)?;
                    let refusal_while_its_node_does_not_run =
                        (why_a_node_that_has_not_failed_does_not_run
                            != WhyANodeOfAStreamIsNotRunning::PlaybackStopped
                            && node.get::<NodeInstanceComponent>().is_none())
                        .then(|| {
                            the_refusal_of_a_reach_for_a_node_not_running(
                                node,
                                why_a_node_that_has_not_failed_does_not_run,
                                format!(
                                    "`GraphInput` node `{}` of the stream `{stream_id}` is not \
                                 running, so the Input `{input_name}` reaches no reader yet",
                                    node.display_name
                                ),
                            )
                        });
                    Ok(TopLevelGraphInputDeclaringAStreamInput {
                        node: node.node_uuid,
                        family: the_family_a_graph_input_carries(node)?,
                        refusal_while_its_node_does_not_run,
                    })
                })
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "the read of a stream's inputs failed to join: {join_failure}"
            ))
        })?
    }
}

/// The top-level `GraphInput` of the stream `stream_id` declaring the input `input_name`.
fn the_top_level_graph_input_declaring<'graph>(
    graph: &'graph Graph,
    stream_id: &str,
    input_name: &str,
) -> Result<&'graph GraphNode> {
    let input_cast = cast_exposed_name_to_url_safe(input_name)?;
    let graph_inputs: Vec<&GraphNode> = graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| node.node_type.as_str() == GRAPH_INPUT_NODE_TYPE)
        .collect();
    let declared_port_of = |node: &GraphNode| -> Option<String> {
        node.config
            .as_ref()?
            .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)?
            .as_str()
            .map(str::to_string)
    };
    if let Some(node) = graph_inputs.iter().find(|node| {
        declared_port_of(node).is_some_and(|port| {
            cast_exposed_name_to_url_safe(&port).is_ok_and(|port_cast| port_cast == input_cast)
        })
    }) {
        return Ok(node);
    }
    let mut declared: Vec<String> = graph_inputs
        .iter()
        .filter_map(|node| declared_port_of(node))
        .map(|port| format!("`{port}`"))
        .collect();
    declared.sort();
    Err(Error::NodeNotFound(format!(
        "the stream `{stream_id}` has no input `{input_name}`; its inputs are {}, each the port a \
         top-level `GraphInput` of family array, channels or records declares. To fix: name one \
         of them. To set an input of a node, name the node",
        if declared.is_empty() {
            "none".to_string()
        } else {
            declared.join(", ")
        }
    )))
}

/// The family the top-level `GraphInput` `node` carries, as its config names it.
fn the_family_a_graph_input_carries(node: &GraphNode) -> Result<PortFamily> {
    node.config
        .as_ref()
        .and_then(|config| config.get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY))
        .and_then(serde_json::Value::as_str)
        .and_then(|wire_name| {
            PortFamily::ALL
                .into_iter()
                .find(|family| family.wire_name() == wire_name)
        })
        .ok_or_else(|| {
            Error::Configuration(format!(
                "`GraphInput` node `{}` names no family",
                node.display_name
            ))
        })
}
