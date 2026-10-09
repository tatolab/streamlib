// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's live exposure map: setting a port's level while the
//! stream runs, and registering the readers from outside the stream that a
//! level change cuts off.

use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use super::LoadedStreamInThisRuntime;
use crate::core::graph::{
    CutOffAReaderOfAnExposedOutputPort, ExposedOutputPortsComponent, Graph,
    GraphNodeWithComponents, OutputPortExposureLevel, OutputPortReaderLocation, ProcessorUniqueId,
    ReaderOfAnExposedOutputPort, cast_exposed_name_to_url_safe, node_names_listed_for_a_refusal,
};
use crate::core::{Error, Result};

static NEXT_EXPOSED_OUTPUT_PORT_READER_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

/// One reader's registration against an exposed output port. Dropping it
/// takes the reader off the port.
#[must_use = "dropping the registration takes the reader off the port at once"]
pub struct ExposedOutputPortReaderRegistration {
    graph_of_the_ports_stream: Weak<RwLock<Graph>>,
    processor_id: ProcessorUniqueId,
    port_name: String,
    registration_id: u64,
}

impl Drop for ExposedOutputPortReaderRegistration {
    fn drop(&mut self) {
        let Some(graph) = self.graph_of_the_ports_stream.upgrade() else {
            return;
        };
        let forgotten_reader = graph
            .write()
            .traversal_mut()
            .v(&self.processor_id)
            .first_mut()
            .and_then(|node| node.get_mut::<ExposedOutputPortsComponent>())
            .and_then(|exposed_ports| {
                exposed_ports.forget_reader(&self.port_name, self.registration_id)
            });
        drop(forgotten_reader);
    }
}

impl LoadedStreamInThisRuntime {
    /// Put output port `port_name` of node `node_name` at `level` while the
    /// stream runs, cutting off at once every reader from outside the stream
    /// the new level no longer allows. Nothing restarts.
    pub fn set_output_port_exposure_level(
        &self,
        node_name: &str,
        port_name: &str,
        level: OutputPortExposureLevel,
    ) -> Result<()> {
        let (port_address, readers_the_level_cuts_off) =
            self.compiler.scope(|graph, _tx| -> Result<_> {
                let (processor_id, port_cast) =
                    self.the_output_port_named(graph, node_name, port_name)?;
                let node = graph
                    .traversal_mut()
                    .v(&processor_id)
                    .first_mut()
                    .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;
                let port_address = format!("{}/{port_cast}", node.display_name);
                if !node.has::<ExposedOutputPortsComponent>() {
                    node.insert_component_without_rendering_it(
                        ExposedOutputPortsComponent::default(),
                    );
                }
                let readers_the_level_cuts_off = node
                    .get_mut::<ExposedOutputPortsComponent>()
                    .map(|exposed_ports| exposed_ports.set_level(&port_cast, level))
                    .unwrap_or_default();
                Ok((port_address, readers_the_level_cuts_off))
            })?;
        tracing::info!(
            stream = %self.stream_name(),
            port = %port_address,
            %level,
            readers_cut_off = readers_the_level_cuts_off.len(),
            "[exposure] port `{port_address}` is now {level}"
        );
        for reader in readers_the_level_cuts_off {
            tracing::info!(
                stream = %self.stream_name(),
                port = %port_address,
                reader = %reader.location(),
                "[exposure] cutting off {} reading `{port_address}`, now {level}",
                reader.location()
            );
            reader.cut_off();
        }
        Ok(())
    }

    /// Register a reader from `location` against output port `port_name` of
    /// node `node_name`, refused naming the port and its level when that level
    /// does not allow a reader there. `cut_off` runs once, after this stream
    /// has let go of its graph, when a later level no longer allows the
    /// reader; it is dropped unrun when the node is removed, and must not hold
    /// the registration this hands back.
    pub fn register_a_reader_of_an_exposed_output_port(
        &self,
        node_name: &str,
        port_name: &str,
        location: OutputPortReaderLocation,
        cut_off: CutOffAReaderOfAnExposedOutputPort,
    ) -> Result<ExposedOutputPortReaderRegistration> {
        let registration_id =
            NEXT_EXPOSED_OUTPUT_PORT_READER_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed);
        let (registered, refused_reader) = self.compiler.scope(|graph, _tx| -> Result<_> {
            let (processor_id, port_cast) =
                self.the_output_port_named(graph, node_name, port_name)?;
            let node = graph
                .traversal_mut()
                .v(&processor_id)
                .first_mut()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;
            let node_display_name = node.display_name.clone();
            let reader = ReaderOfAnExposedOutputPort::new(registration_id, location, cut_off);
            let outcome = match node.get_mut::<ExposedOutputPortsComponent>() {
                Some(exposed_ports) => exposed_ports.register_reader(&port_cast, reader),
                None => Err((reader, OutputPortExposureLevel::Internal)),
            };
            Ok(match outcome {
                Ok(()) => (
                    Ok(ExposedOutputPortReaderRegistration {
                        graph_of_the_ports_stream: self.compiler.graph_held_weakly(),
                        processor_id,
                        port_name: port_cast,
                        registration_id,
                    }),
                    None,
                ),
                Err((refused_reader, level)) => (
                    Err(Error::OutputPortNotExposedToTheReader {
                        stream: self.stream_name().to_string(),
                        node: node_display_name,
                        port: port_cast,
                        level: level.to_string(),
                        reader: location.to_string(),
                        levels_the_reader_may_read: location
                            .levels_a_reader_here_may_read()
                            .to_string(),
                    }),
                    Some(refused_reader),
                ),
            })
        })?;
        drop(refused_reader);
        registered
    }

    /// The node `node_name` names in `graph` and the cast name of its output
    /// port `port_name`, each refused by name when it is not there.
    fn the_output_port_named(
        &self,
        graph: &Graph,
        node_name: &str,
        port_name: &str,
    ) -> Result<(ProcessorUniqueId, String)> {
        let traversal = graph.traversal();
        let node = traversal
            .v_with_node_name(node_name)
            .first()
            .ok_or_else(|| {
                Error::GraphError(format!(
                    "stream `{}` holds no node `{node_name}`. It holds: {}",
                    self.stream_name(),
                    node_names_listed_for_a_refusal(
                        graph
                            .traversal()
                            .v(())
                            .iter()
                            .map(|node| node.display_name.as_str())
                    )
                ))
            })?;
        let port_cast = cast_exposed_name_to_url_safe(port_name)?.into_owned();
        if !node
            .ports
            .outputs
            .iter()
            .any(|output| output.name == port_cast)
        {
            let output_names: Vec<&str> = node
                .ports
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect();
            return Err(Error::GraphError(format!(
                "node `{}` in stream `{}` has no output port `{port_name}`. Its output \
                 ports are: {}",
                node.display_name,
                self.stream_name(),
                if output_names.is_empty() {
                    "none".to_string()
                } else {
                    output_names.join(", ")
                }
            )));
        }
        Ok((node.id.clone(), port_cast))
    }
}
