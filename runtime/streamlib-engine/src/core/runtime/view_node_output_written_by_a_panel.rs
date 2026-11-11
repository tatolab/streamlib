// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A value a client writes onto an output of a node whose outputs a client writes — a view node,
//! or a stream's top-level `GraphInput` — handed to the node, which publishes it on that port.
//! Live data only, so the graph and its document never change for it.

use std::sync::Arc;

use parking_lot::Mutex;

use super::LoadedStreamInThisRuntime;
use super::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;
use crate::core::descriptors::PortFamily;
use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{Graph, GraphNodeWithComponents, ProcessorInstanceComponent};
use crate::core::graph_interface_resolution::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_FAMILY_CONFIG_KEY, GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME,
};
use crate::core::processors::{
    NodeTypesOneStreamResolves, NodeWhoseOutputsAClientWritesProcessor, ProcessorInstance,
};

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
        let node_types = Arc::clone(self.node_types_this_stream_resolves());
        let stream_id = self.stream_id().to_string();
        self.publish_a_value_on_an_output_of_the_running_node(
            port_name.clone(),
            value,
            move |graph, why_a_node_that_has_not_failed_does_not_run| {
                let (node_name, processor_instance) = the_running_view_node_of_the_id(
                    graph,
                    &node_types,
                    &stream_id,
                    &node_id,
                    &port_name,
                    why_a_node_that_has_not_failed_does_not_run,
                )?;
                Ok((node_name, (), processor_instance))
            },
        )
        .await
    }

    /// Hand `value` to this stream's top-level `GraphInput` declaring the Input `input_name`,
    /// which publishes it on its `value` output to every reader, answering the Input's family.
    /// Refused for a name no running top-level `GraphInput` declares.
    pub(crate) async fn hand_a_value_written_to_a_top_level_graph_input(
        &self,
        input_name: String,
        value: serde_json::Value,
    ) -> Result<PortFamily> {
        let stream_id = self.stream_id().to_string();
        self.publish_a_value_on_an_output_of_the_running_node(
            GRAPH_INTERFACE_VALUE_PORT_NAME.to_string(),
            value,
            move |graph, why_a_node_that_has_not_failed_does_not_run| {
                the_running_top_level_graph_input_declaring(
                    graph,
                    &stream_id,
                    &input_name,
                    why_a_node_that_has_not_failed_does_not_run,
                )
            },
        )
        .await
    }

    /// Publish `value` on output `port_name` of the node `find_the_node` finds in the stream's
    /// graph, handed why a node that has not failed does not run now, answering what it found
    /// beside the node's name and processor. The stream's graph lock is held across a compile,
    /// so the write waits off the async workers.
    async fn publish_a_value_on_an_output_of_the_running_node<
        FoundBesideTheNode: Send + 'static,
    >(
        &self,
        port_name: String,
        value: serde_json::Value,
        find_the_node: impl FnOnce(
            &Graph,
            WhyANodeOfAStreamIsNotRunning,
        ) -> Result<(
            String,
            FoundBesideTheNode,
            Arc<Mutex<ProcessorInstance>>,
        )> + Send
        + 'static,
    ) -> Result<FoundBesideTheNode> {
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
        let compiler = Arc::clone(&self.compiler);
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let (node_name, found_beside_the_node, processor_instance) =
                    compiler.scope(|graph, _tx| {
                        find_the_node(graph, why_a_node_that_has_not_failed_does_not_run)
                    })?;
                let mut processor_instance = processor_instance.lock();
                let node_whose_outputs_a_client_writes = processor_instance
                    .as_any_mut()
                    .downcast_mut::<NodeWhoseOutputsAClientWritesProcessor>()
                    .ok_or_else(|| {
                        Error::Runtime(format!(
                            "node `{node_name}` runs a processor whose outputs no client writes"
                        ))
                    })?;
                node_whose_outputs_a_client_writes
                    .take_a_value_a_client_wrote(&port_name, &value)?;
                Ok(found_beside_the_node)
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

/// The name, family and processor of the running top-level `GraphInput` declaring the Input
/// `input_name`, matched as its config's `port` casts.
fn the_running_top_level_graph_input_declaring(
    graph: &Graph,
    stream_id: &str,
    input_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> Result<(String, PortFamily, Arc<Mutex<ProcessorInstance>>)> {
    let input_cast = crate::core::graph::cast_exposed_name_to_url_safe(input_name)?;
    let graph_inputs: Vec<_> = graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| node.processor_type.as_str() == GRAPH_INPUT_NODE_TYPE)
        .collect();
    let declared_port_of = |node: &crate::core::graph::ProcessorNode| -> Option<String> {
        node.config
            .as_ref()?
            .get(GRAPH_INTERFACE_PORT_CONFIG_KEY)?
            .as_str()
            .map(str::to_string)
    };
    let Some(node) = graph_inputs.iter().find(|node| {
        declared_port_of(node).is_some_and(|port| {
            crate::core::graph::cast_exposed_name_to_url_safe(&port)
                .is_ok_and(|port_cast| port_cast == input_cast)
        })
    }) else {
        let mut declared: Vec<String> = graph_inputs
            .iter()
            .filter_map(|node| declared_port_of(node))
            .map(|port| format!("`{port}`"))
            .collect();
        declared.sort();
        return Err(Error::ProcessorNotFound(format!(
            "no `GraphInput` on the top level of the stream `{stream_id}` declares the Input \
             `{input_name}` and runs; a client writes the Inputs of family array, channels or \
             records, and the stream's are: {}",
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            }
        )));
    };
    let family = node
        .config
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
        })?;
    let processor_instance = node
        .get::<ProcessorInstanceComponent>()
        .map(|instance| Arc::clone(&instance.0))
        .ok_or_else(|| {
            the_refusal_of_a_reach_for_a_node_not_running(
                node,
                why_a_node_that_has_not_failed_does_not_run,
                format!(
                    "`GraphInput` node `{}` of the stream `{stream_id}` is not running, so the \
                     Input `{input_name}` reaches no reader yet",
                    node.display_name
                ),
            )
        })?;
    Ok((node.display_name.clone(), family, processor_instance))
}

/// The name and processor of the running node of a view type whose UUID is `node_id`.
fn the_running_view_node_of_the_id(
    graph: &Graph,
    node_types: &NodeTypesOneStreamResolves,
    stream_id: &str,
    node_id: &str,
    port_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
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
    Ok((node.display_name.clone(), processor_instance))
}

#[cfg(test)]
#[path = "view_node_output_written_by_a_panel_tests.rs"]
mod view_node_output_written_by_a_panel_tests;
