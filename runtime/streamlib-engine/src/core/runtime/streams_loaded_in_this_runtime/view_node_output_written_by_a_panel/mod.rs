// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The view node a panel writes onto an output of: the node whose input of that output's name a
//! live `set_node_input_value` sets, which the node publishes on the output. Live data only, so
//! the graph and its document never change for it.

use std::sync::Arc;

use uuid::Uuid;

use crate::core::descriptors::PinType;
use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{Graph, GraphNode, GraphNodeWithComponents, NodeInstanceComponent};
use crate::core::nodes::{the_names_listed, the_refusal_of_a_value_an_output_does_not_carry};
use crate::core::pin_value_packing::pack_pin_value;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;
use streamlib_project_repository::a_panel_writes;

impl LoadedStreamInThisRuntime {
    /// The UUID of the running view node whose UUID, as the stream's graph lists it under `id`,
    /// is `node_id`, and one of whose outputs a panel writes `value` onto, as `port_name`: the
    /// node whose input of that output's name a write sets. Refused for a stream whose project
    /// is stopped; a node that is not of a view type, that does not run, or that has no such
    /// output; and a value no panel writes there: any onto a Texture or encoded output, one its
    /// type does not take, and `null`. The stream's graph lock is held across a compile, so the read waits off the async
    /// workers.
    pub(crate) async fn read_the_running_view_node_a_panel_writes(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> Result<Uuid> {
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
        let compiler = Arc::clone(&self.compiler);
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                compiler.scope_reading(|graph| {
                    let (node, pin_type) = the_running_view_node_of_the_id(
                        graph,
                        &stream_id,
                        &node_id,
                        &port_name,
                        why_a_node_that_has_not_failed_does_not_run,
                    )?;
                    refuse_a_value_no_panel_writes_onto_the_output(
                        &node.display_name,
                        &port_name,
                        &pin_type,
                        &value,
                    )?;
                    Ok(node.node_uuid)
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
    stream_id: &str,
    node_id: &str,
    port_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> Result<(&'graph GraphNode, PinType)> {
    let no_such_node = || {
        Error::NodeNotFound(format!(
            "no node in the stream `{stream_id}` has the id `{node_id}`"
        ))
    };
    let node_uuid = Uuid::parse_str(node_id).map_err(|_| no_such_node())?;
    let node = graph.node_by_uuid(node_uuid).ok_or_else(no_such_node)?;
    if !graph
        .node_types_this_stream_resolves()
        .is_a_view_type_of_this_streams_project(&node.node_type)
    {
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
    let Some(output) = node.output_port(port_name) else {
        return Err(Error::PortError(format!(
            "node `{}` has no output `{port_name}`; its outputs are {}",
            node.display_name,
            the_names_listed(node.ports.outputs.iter().map(|port| &port.name))
        )));
    };
    let Some(pin) = &output.pin else {
        return Err(Error::PortError(format!(
            "output `{port_name}` of node `{}` carries no pin type, so no panel writes it",
            node.display_name
        )));
    };
    Ok((node, pin.pin_type.clone()))
}

/// Refuse `value` written onto output `port_name`, of `pin_type`, of the view node named
/// `node_name` when no panel writes it there: any value onto an output of a type no panel writes,
/// one that type does not take, and `null`, which sets no value; each in the words of the
/// message the type travels as.
fn refuse_a_value_no_panel_writes_onto_the_output(
    node_name: &str,
    port_name: &str,
    pin_type: &PinType,
    value: &serde_json::Value,
) -> Result<()> {
    let why = if value.is_null() {
        "`null` sets no value".to_string()
    } else if !a_panel_writes(pin_type) {
        format!("a panel writes no {pin_type} value; a node writes it")
    } else {
        match pack_pin_value(pin_type, value, 0) {
            Ok(_) => return Ok(()),
            Err(why) => why,
        }
    };
    Err(the_refusal_of_a_value_an_output_does_not_carry(
        node_name, port_name, pin_type, &why,
    ))
}

#[cfg(test)]
mod tests;
