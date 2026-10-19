// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's live exposure map: setting a port's level while the
//! stream runs, and the one refusal of a reader from outside the stream that a
//! port's level does not allow.

use super::LoadedStreamInThisRuntime;
use crate::core::error::OutputPortNotExposedToTheReader;
use crate::core::graph::{
    ExposedOutputPortsComponent, Graph, GraphNodeWithComponents, OutputPortExposureLevel,
    OutputPortReaderOutsideItsStream, ProcessorNode, cast_exposed_name_to_url_safe,
    node_named_or_refused, output_port_exposure_allows_the_reader, port_names_listed_for_a_refusal,
};
use crate::core::pubsub::OutputPortExposureChange;
use crate::core::{Error, Result};

impl LoadedStreamInThisRuntime {
    /// Put output port `port_name` of node `node_name` at `level` while the
    /// stream runs, saying so on
    /// [`topics::OUTPUT_PORT_EXPOSURE`](crate::core::pubsub::topics::OUTPUT_PORT_EXPOSURE).
    /// Nothing restarts.
    pub fn set_output_port_exposure_level(
        &self,
        node_name: &str,
        port_name: &str,
        level: OutputPortExposureLevel,
    ) -> Result<()> {
        let (node_display_name, port_cast) = self.compiler.scope(|graph, _tx| -> Result<_> {
            let (node, port_cast) = self.the_output_port_named(graph, node_name, port_name)?;
            if level != OutputPortExposureLevel::Internal
                && !node.has::<ExposedOutputPortsComponent>()
            {
                node.insert_component_without_rendering_it(ExposedOutputPortsComponent::default());
            }
            if let Some(exposed_ports) = node.get_mut::<ExposedOutputPortsComponent>() {
                exposed_ports.set_level(&port_cast, level);
            }
            Ok((node.display_name.clone(), port_cast))
        })?;
        let port_address = format!("{node_display_name}/{port_cast}");
        tracing::debug!(
            stream = %self.stream_name(),
            port = %port_address,
            %level,
            "[exposure] port `{port_address}` is now {level}"
        );
        OutputPortExposureChange::LevelSet {
            stream_name: self.stream_name().to_string(),
            node_name: node_display_name,
            port_name: port_cast,
            level,
        }
        .publish();
        Ok(())
    }

    /// The level output port `port_name` of node `node_name` is at now, each
    /// refused by name when it is not there.
    pub fn output_port_exposure_level(
        &self,
        node_name: &str,
        port_name: &str,
    ) -> Result<OutputPortExposureLevel> {
        self.compiler.scope(|graph, _tx| {
            let (node, port_cast) = self.the_output_port_named(graph, node_name, port_name)?;
            Ok(exposure_level_of_output_port(node, &port_cast))
        })
    }

    /// The node `node_name` names in `graph` and the cast name of its output
    /// port `port_name`, each refused by name when it is not there.
    fn the_output_port_named<'graph>(
        &self,
        graph: &'graph mut Graph,
        node_name: &str,
        port_name: &str,
    ) -> Result<(&'graph mut ProcessorNode, String)> {
        the_output_port_named_on_this_graph(graph, self.stream_name(), node_name, port_name)
    }
}

/// Refuse a reader from `location` of output port `port_name` of node
/// `node_name` on `graph`, the graph of the stream `stream_name`, naming the
/// port and its level, when that level does not allow a reader there.
pub(crate) fn refuse_a_reader_the_port_does_not_allow(
    graph: &mut Graph,
    stream_name: &str,
    node_name: &str,
    port_name: &str,
    location: OutputPortReaderOutsideItsStream,
) -> Result<()> {
    let (node, port_cast) =
        the_output_port_named_on_this_graph(graph, stream_name, node_name, port_name)?;
    let level = exposure_level_of_output_port(node, &port_cast);
    if output_port_exposure_allows_the_reader(level, location.location()) {
        return Ok(());
    }
    Err(a_reader_the_port_does_not_allow(
        stream_name,
        &node.display_name,
        port_cast,
        level,
        location,
    ))
}

/// The level output port `port_cast` of `node` is at: internal unless the
/// node exposes it.
fn exposure_level_of_output_port(node: &ProcessorNode, port_cast: &str) -> OutputPortExposureLevel {
    node.get::<ExposedOutputPortsComponent>()
        .map_or(OutputPortExposureLevel::Internal, |exposed_ports| {
            exposed_ports.level_of(port_cast)
        })
}

/// The one refusal of a reader an output port's level does not allow.
fn a_reader_the_port_does_not_allow(
    stream_name: &str,
    node_display_name: &str,
    port_cast: String,
    level: OutputPortExposureLevel,
    location: OutputPortReaderOutsideItsStream,
) -> Error {
    Error::OutputPortNotExposedToTheReader(Box::new(OutputPortNotExposedToTheReader {
        stream: stream_name.to_string(),
        node: node_display_name.to_string(),
        port: port_cast,
        level: level.to_string(),
        reader: location.to_string(),
        levels_the_reader_may_read: location.location().levels_a_reader_here_may_read(),
    }))
}

/// The node `node_name` names in `graph`, the graph of the stream
/// `stream_name`, and the cast name of its output port `port_name`, each
/// refused by name — listing what the stream holds — when it is not there.
pub(crate) fn the_output_port_named_on_this_graph<'graph>(
    graph: &'graph mut Graph,
    stream_name: &str,
    node_name: &str,
    port_name: &str,
) -> Result<(&'graph mut ProcessorNode, String)> {
    let named = node_named_or_refused(graph, stream_name, node_name)?;
    let port_cast = cast_exposed_name_to_url_safe(port_name)?.into_owned();
    if !named.has_output(&port_cast) {
        return Err(Error::NodeInAStreamHasNoOutputPortNamed {
            stream: stream_name.to_string(),
            node: named.display_name.clone(),
            port: port_name.to_string(),
            output_ports_the_node_has: port_names_listed_for_a_refusal(
                named
                    .ports
                    .outputs
                    .iter()
                    .map(|output| output.name.as_str()),
            ),
        });
    }
    let processor_id = named.id.clone();
    graph
        .traversal_mut()
        .v(&processor_id)
        .first_mut()
        .map(|node| (node, port_cast))
        .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))
}
