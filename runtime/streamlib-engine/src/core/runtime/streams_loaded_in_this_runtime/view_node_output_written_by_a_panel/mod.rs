// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value a panel writes onto an output of a view node, handed to the node, which publishes it
//! on that port. Live data only, so the graph and its document never change for it.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{Graph, GraphNodeWithComponents, NodeInstanceComponent};
use crate::core::nodes::{NodeInstance, NodeTypesOneStreamResolves, ValueSourceNode};
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;

impl LoadedStreamInThisRuntime {
    /// Hand `value` to the view node whose UUID, as the stream's graph lists it under `id`, is
    /// `node_id`, which publishes it on its output `port_name`. Refused for a stream whose project
    /// is stopped, and a node that is not of a view type, that does not run, or whose port does
    /// not take the value. The stream's graph
    /// lock is held across a compile, so the write waits off the async workers.
    pub(crate) async fn hand_a_value_written_by_a_panel_to_a_view_node(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> Result<()> {
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
        let node_types = Arc::clone(self.node_types_this_stream_resolves());
        let stream_id = self.stream_id().to_string();
        let compiler = Arc::clone(&self.compiler);
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let (node_name, node_instance) = compiler.scope(|graph, _tx| {
                    the_running_view_node_of_the_id(
                        graph,
                        &node_types,
                        &stream_id,
                        &node_id,
                        &port_name,
                        why_a_node_that_has_not_failed_does_not_run,
                    )
                })?;
                let mut node_instance = node_instance.lock();
                let value_source = node_instance
                    .as_any_mut()
                    .downcast_mut::<ValueSourceNode>()
                    .ok_or_else(|| {
                        Error::Runtime(format!("node `{node_name}` runs no value source"))
                    })?;
                value_source.publish_a_value_written_onto_an_output(&port_name, &value)
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a write onto a node's output failed to join: {join_failure}"
            ))
        })?
    }
}

fn the_running_view_node_of_the_id(
    graph: &Graph,
    node_types: &NodeTypesOneStreamResolves,
    stream_id: &str,
    node_id: &str,
    port_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> Result<(String, Arc<Mutex<NodeInstance>>)> {
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
    let node_instance = node
        .get::<NodeInstanceComponent>()
        .map(|instance| Arc::clone(&instance.0))
        .ok_or_else(|| {
            the_refusal_of_a_reach_for_a_node_not_running(
                node,
                why_a_node_that_has_not_failed_does_not_run,
                format!(
                    "view node `{node_id}` ({}) of the stream `{stream_id}` is not running, so \
                     its output `{port_name}` publishes nothing yet",
                    node.display_name
                ),
            )
        })?;
    Ok((node.display_name.clone(), node_instance))
}

#[cfg(test)]
mod tests;
