// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A value a panel writes onto an output of a view node: handed to the node, which publishes it
//! on that port. Live data only, so the graph and its document never change for it.

use std::sync::Arc;

use parking_lot::Mutex;

use super::LoadedStreamInThisRuntime;
use crate::core::error::{Error, Result};
use crate::core::graph::{Graph, GraphNodeWithComponents, ProcessorInstanceComponent};
use crate::core::processors::{
    NodeTypesOneStreamResolves, ProcessorInstance, ProjectViewNodeProcessor,
};

impl LoadedStreamInThisRuntime {
    /// Hand `value` to the view node whose UUID, as the stream's graph lists it under `id`, is
    /// `node_id`, which publishes it on its output `port_name`. Refused for a node that is not of
    /// a view type, that does not run, or whose port does not take the value. The stream's graph
    /// lock is held across a compile, so the write waits off the async workers.
    pub(crate) async fn hand_a_value_written_by_a_panel_to_a_view_node(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> Result<()> {
        let compiler = Arc::clone(&self.compiler);
        let node_types = Arc::clone(self.node_types_this_stream_resolves());
        let stream_id = self.stream_id().to_string();
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let (node_name, processor_instance) =
                    compiler.scope(|graph, _tx| -> Result<_> {
                        the_running_view_node_of_the_id(
                            graph,
                            &node_types,
                            &stream_id,
                            &node_id,
                            &port_name,
                        )
                    })?;
                let mut processor_instance = processor_instance.lock();
                let view_node = processor_instance
                    .as_any_mut()
                    .downcast_mut::<ProjectViewNodeProcessor>()
                    .ok_or_else(|| {
                        Error::Runtime(format!(
                            "view node `{node_id}` ({node_name}) runs a processor that is no \
                             view node's"
                        ))
                    })?;
                view_node.publish_a_value_written_onto_an_output(&port_name, &value)
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a write onto a view node's output failed to join: {join_failure}"
            ))
        })?
    }
}

/// The name and processor of the running node of a view type whose UUID is `node_id`.
fn the_running_view_node_of_the_id(
    graph: &Graph,
    node_types: &NodeTypesOneStreamResolves,
    stream_id: &str,
    node_id: &str,
    port_name: &str,
) -> Result<(String, Arc<Mutex<ProcessorInstance>>)> {
    let Some(node) = graph
        .traversal()
        .v(())
        .iter()
        .find(|node| node.node_uuid.to_string() == node_id)
    else {
        return Err(Error::ProcessorNotFound(format!(
            "no node in the stream `{stream_id}` has the id `{node_id}`"
        )));
    };
    if !node_types.is_a_view_type_of_this_streams_project(&node.processor_type) {
        return Err(Error::Configuration(format!(
            "node `{node_id}` ({}) of the stream `{stream_id}` is of the type `{}`, which is no \
             view its project brings, so no panel writes its outputs",
            node.display_name, node.processor_type
        )));
    }
    let processor_instance = node
        .get::<ProcessorInstanceComponent>()
        .map(|instance| Arc::clone(&instance.0))
        .ok_or_else(|| {
            Error::Link(format!(
                "view node `{node_id}` ({}) of the stream `{stream_id}` is not running, so its \
                 output `{port_name}` publishes nothing yet",
                node.display_name
            ))
        })?;
    Ok((node.display_name.clone(), processor_instance))
}

#[cfg(test)]
#[path = "view_node_output_written_by_a_panel_tests.rs"]
mod view_node_output_written_by_a_panel_tests;
