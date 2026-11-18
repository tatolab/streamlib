// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The view node a panel writes onto an output of: the node whose input of that output's name a
//! live `set_node_input_value` sets, which the node publishes on the output. Live data only, so
//! the graph and its document never change for it.

use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{Graph, GraphNode, GraphNodeWithComponents, NodeInstanceComponent};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;

impl LoadedStreamInThisRuntime {
    /// The UUID of the running view node whose UUID, as the stream's graph lists it under `id`,
    /// is `node_id`, and one of whose outputs is `port_name`: the node a panel's write onto that
    /// output sets the same-named input of. Refused for a stream whose project is stopped, and a
    /// node that is not of a view type, that does not run, or that has no such output. The
    /// stream's graph lock is held across a compile, so the read waits off the async workers.
    pub(crate) async fn read_the_running_view_node_a_panel_writes(
        &self,
        node_id: String,
        port_name: String,
    ) -> Result<uuid::Uuid> {
        let why_a_node_that_has_not_failed_does_not_run =
            self.why_a_node_of_this_stream_that_has_not_failed_does_not_run();
        if why_a_node_that_has_not_failed_does_not_run
            == WhyANodeOfAStreamIsNotRunning::PlaybackStopped
        {
            return Err(Error::NodeOfAStreamIsNotRunning {
                not_running_because: WhyANodeOfAStreamIsNotRunning::PlaybackStopped,
                runtime_words: format!(
                    "the project of the stream `{}` in {} is stopped, so no node of it runs to \
                     publish on `{port_name}`; `start_playback` plays it",
                    self.stream_id(),
                    self.project_directory().display()
                ),
            });
        }
        let stream_id = self.stream_id().to_string();
        let compiler = std::sync::Arc::clone(&self.compiler);
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                compiler.scope_reading(|graph| {
                    let node_types = graph.node_types_this_stream_resolves();
                    the_running_view_node_of_the_id(
                        graph,
                        node_types,
                        &stream_id,
                        &node_id,
                        &port_name,
                        why_a_node_that_has_not_failed_does_not_run,
                    )
                    .map(|node| node.node_uuid)
                })
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "the read of the view node a panel writes failed to join: {join_failure}"
            ))
        })?
    }
}

fn the_running_view_node_of_the_id<'graph>(
    graph: &'graph Graph,
    node_types: &NodeTypesOneStreamResolves,
    stream_id: &str,
    node_id: &str,
    port_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> Result<&'graph GraphNode> {
    let Some(node) = graph
        .traversal()
        .v(())
        .iter()
        .find(|node| node.node_uuid.to_string() == node_id)
    else {
        return Err(Error::NodeNotFound(format!(
            "no node in the stream `{stream_id}` has the id `{node_id}`"
        )));
    };
    if !node_types.is_a_view_type_of_this_streams_project(&node.node_type) {
        return Err(Error::Configuration(format!(
            "node `{node_id}` ({}) of the stream `{stream_id}` is of the type `{}`, which is no \
             view its project brings, so no panel writes its outputs",
            node.display_name, node.node_type
        )));
    }
    if node.get::<NodeInstanceComponent>().is_none() {
        return Err(the_refusal_of_a_reach_for_a_node_not_running(
            node,
            why_a_node_that_has_not_failed_does_not_run,
            format!(
                "view node `{node_id}` ({}) of the stream `{stream_id}` is not running, so its \
                 output `{port_name}` publishes nothing yet",
                node.display_name
            ),
        ));
    }
    if !node.has_output(port_name) {
        let mut outputs: Vec<String> = node
            .ports
            .outputs
            .iter()
            .map(|port| format!("`{}`", port.name))
            .collect();
        outputs.sort();
        return Err(Error::PortError(format!(
            "node `{}` has no output `{port_name}`; its outputs are {}",
            node.display_name,
            if outputs.is_empty() {
                "none".to_string()
            } else {
                outputs.join(", ")
            }
        )));
    }
    Ok(node)
}

#[cfg(test)]
mod tests;
