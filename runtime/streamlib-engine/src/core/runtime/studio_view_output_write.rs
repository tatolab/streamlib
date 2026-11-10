// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A value a panel writes onto a StudioView node's output, handed to the node that publishes it.

use std::sync::Arc;

use parking_lot::Mutex;

use super::LoadedStreamInThisRuntime;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorInstanceComponent, names_listed_for_a_refusal,
};
use crate::core::processors::ProcessorInstance;
use crate::core::studio_view::{StudioViewProcessor, is_the_studio_view_node_type};

impl LoadedStreamInThisRuntime {
    /// Hand `value` to the running StudioView node whose UUID is `node_id`, to publish on its
    /// output `port_name`. The graph is read and never changed, so nothing written is recorded.
    pub(crate) async fn hand_a_studio_view_node_a_value_written_onto_its_output(
        &self,
        node_id: &str,
        port_name: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        let compiler = Arc::clone(&self.compiler);
        let stream_id = self.stream_id().to_string();
        let node_id = node_id.to_string();
        let port_name = port_name.to_string();
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let processor_instance = compiler.scope(|graph, _tx| {
                    the_processor_of_the_running_studio_view_node(graph, &stream_id, &node_id)
                })?;
                let mut processor_instance = processor_instance.lock();
                let studio_view = processor_instance
                    .as_any_mut()
                    .downcast_mut::<StudioViewProcessor>()
                    .ok_or_else(|| {
                        Error::Runtime(format!(
                            "node `{node_id}` of the stream `{stream_id}` runs no StudioView \
                             processor"
                        ))
                    })?;
                studio_view.publish_a_value_written_onto_an_output(&port_name, &value)
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a write onto a StudioView output failed to join: {join_failure}"
            ))
        })?
    }
}

/// The processor of the StudioView node of `graph` whose UUID is `node_id`, refused unless the
/// node is a StudioView that runs.
fn the_processor_of_the_running_studio_view_node(
    graph: &Graph,
    stream_id: &str,
    node_id: &str,
) -> Result<Arc<Mutex<ProcessorInstance>>> {
    let Some(node) = graph
        .traversal()
        .v(())
        .iter()
        .find(|node| node.node_uuid.to_string() == node_id)
    else {
        return Err(Error::ProcessorNotFound(format!(
            "no node in the stream `{stream_id}` has the id `{node_id}`. The stream \
             `{stream_id}` holds: {}",
            names_listed_for_a_refusal(
                graph
                    .traversal()
                    .v(())
                    .iter()
                    .map(|node| format!("`{}` ({})", node.node_uuid, node.display_name)),
                "no node",
            )
        )));
    };
    if !is_the_studio_view_node_type(&node.processor_type) {
        return Err(Error::Configuration(format!(
            "node `{node_id}` ({}) of the stream `{stream_id}` is a `{}`; a value is written \
             onto a StudioView node's output",
            node.display_name, node.processor_type
        )));
    }
    node.get::<ProcessorInstanceComponent>()
        .filter(|_| node.runs_its_processor())
        .map(|instance| Arc::clone(&instance.0))
        .ok_or_else(|| {
            Error::Link(format!(
                "node `{node_id}` ({}) of the stream `{stream_id}` is not running, so it \
                 publishes nothing yet",
                node.display_name
            ))
        })
}
