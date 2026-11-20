// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's input, named by the port its top-level `GraphInput` declares: the node whose
//! `value` input a set naming no node sets.

use uuid::Uuid;

use crate::core::descriptors::PortFamily;
use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_INTERFACE_VALUE_PORT_NAME,
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
    pub(crate) async fn read_the_top_level_graph_input_declaring(
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
                    let node = the_graph_input_declaring_the_input(graph, &stream_id, &input_name)?;
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
fn the_graph_input_declaring_the_input<'graph>(
    graph: &'graph Graph,
    stream_id: &str,
    input_name: &str,
) -> Result<&'graph GraphNode> {
    let input_cast = cast_exposed_name_to_url_safe(input_name)?;
    let graph_inputs: Vec<&GraphNode> = graph.every_graph_input().collect();
    let declared_port_of = |node: &GraphNode| -> Option<String> {
        node.input_values()
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
    let mut inputs_the_stream_has: Vec<String> = graph_inputs
        .iter()
        .filter_map(|node| declared_port_of(node))
        .collect();
    inputs_the_stream_has.sort();
    Err(Error::StreamHasNoInputNamed {
        stream: stream_id.to_string(),
        input: input_name.to_string(),
        inputs_the_stream_has,
    })
}

/// The family the top-level `GraphInput` `node` carries, as its `value` input resolved it from
/// its settings.
fn the_family_a_graph_input_carries(node: &GraphNode) -> Result<PortFamily> {
    node.input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .map(|value| value.family.family)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "`GraphInput` node `{}` has no `{GRAPH_INTERFACE_VALUE_PORT_NAME}` input",
                node.display_name
            ))
        })
}
