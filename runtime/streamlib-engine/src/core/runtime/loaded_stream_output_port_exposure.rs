// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's live exposure map: setting a port's level while the
//! stream runs, and registering the readers from outside the stream that a
//! level change cuts off.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::LoadedStreamInThisRuntime;
use crate::core::error::OutputPortNotExposedToTheReader;
use crate::core::graph::{
    CutOffAReaderOfAnExposedOutputPort, ExposedOutputPortsComponent, Graph,
    GraphNodeWithComponents, OutputPortExposureLevel, OutputPortReaderOutsideItsStream,
    ProcessorNode, ProcessorUniqueId, ReaderOfAnExposedOutputPort,
    WhyAReaderOfAnExposedOutputPortIsCutOff, cast_exposed_name_to_url_safe,
    node_names_listed_for_a_refusal, output_port_exposure_allows_the_reader,
    port_names_listed_for_a_refusal,
};
use crate::core::{Error, Result};

/// One reader's registration against an exposed output port. Dropping it
/// takes the reader off the port: a cut not yet begun never begins, though one
/// already under way is not waited for, and the port lets the reader go at its
/// next change. Dropping it takes no lock.
#[derive(Debug)]
#[must_use = "dropping the registration takes the reader off the port at once"]
pub struct ExposedOutputPortReaderRegistration {
    still_registered: Arc<AtomicBool>,
}

impl Drop for ExposedOutputPortReaderRegistration {
    fn drop(&mut self) {
        self.still_registered.store(false, Ordering::Release);
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
        let (port_address, readers_leaving_the_port) =
            self.compiler.scope(|graph, _tx| -> Result<_> {
                let (node, port_cast) = self.the_output_port_named(graph, node_name, port_name)?;
                let port_address = format!("{}/{port_cast}", node.display_name);
                if level != OutputPortExposureLevel::Internal
                    && !node.has::<ExposedOutputPortsComponent>()
                {
                    node.insert_component_without_rendering_it(
                        ExposedOutputPortsComponent::default(),
                    );
                }
                let readers_leaving_the_port = node
                    .get_mut::<ExposedOutputPortsComponent>()
                    .map(|exposed_ports| exposed_ports.set_level(&port_cast, level))
                    .unwrap_or_default();
                Ok((port_address, readers_leaving_the_port))
            })?;
        tracing::debug!(
            stream = %self.stream_name(),
            port = %port_address,
            %level,
            "[exposure] port `{port_address}` is now {level}"
        );
        for reader in readers_leaving_the_port {
            if !reader.is_still_registered() {
                continue;
            }
            tracing::info!(
                stream = %self.stream_name(),
                port = %port_address,
                reader = %reader.location(),
                "[exposure] cutting off {} reading `{port_address}`, now {level}",
                reader.location()
            );
            let cut_off = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reader.cut_off_unless_its_registration_was_dropped(
                    WhyAReaderOfAnExposedOutputPortIsCutOff::TheLevelNoLongerAllowsIt { level },
                )
            }));
            if cut_off.is_err() {
                tracing::error!(
                    stream = %self.stream_name(),
                    port = %port_address,
                    "[exposure] a reader's cut off `{port_address}` panicked; the readers after \
                     it are still cut off"
                );
            }
        }
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
            Ok(node
                .get::<ExposedOutputPortsComponent>()
                .map_or(OutputPortExposureLevel::Internal, |exposed_ports| {
                    exposed_ports.level_of(&port_cast)
                }))
        })
    }

    /// Register a reader from `location` against output port `port_name` of
    /// node `node_name`, refused naming the port and its level when that level
    /// does not allow a reader there. `cut_off` runs at most once, outside
    /// every graph lock, when a later level no longer allows the reader. It
    /// never begins once the registration this hands back is dropped, but a
    /// drop racing a cut already under way does not wait for it, so a cut must
    /// tolerate its reader being gone.
    pub fn register_a_reader_of_an_exposed_output_port(
        &self,
        node_name: &str,
        port_name: &str,
        location: OutputPortReaderOutsideItsStream,
        cut_off: CutOffAReaderOfAnExposedOutputPort,
    ) -> Result<ExposedOutputPortReaderRegistration> {
        let (registered, readers_to_drop_outside_the_graph_lock) =
            self.compiler.scope(|graph, _tx| {
                register_a_reader_on_this_graph(
                    graph,
                    self.stream_name(),
                    node_name,
                    port_name,
                    location,
                    cut_off,
                )
            });
        // A reader's cut may own anything, so it is dropped only once the graph lock is released.
        drop(readers_to_drop_outside_the_graph_lock);
        registered.map(|registered| registered.registration)
    }

    /// Refuse a reader from `location` of output port `port_name` of node
    /// `node_name`, in the words registering it would be refused in, without
    /// registering anything.
    pub fn refuse_a_reader_its_output_port_does_not_allow(
        &self,
        node_name: &str,
        port_name: &str,
        location: OutputPortReaderOutsideItsStream,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| {
            refuse_a_reader_the_port_does_not_allow(
                graph,
                self.stream_name(),
                node_name,
                port_name,
                location,
            )
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

/// A reader registered against an output port on a graph: its registration,
/// and the processor and cast port name it reads.
pub(crate) struct ReaderRegisteredOnAGraph {
    pub(crate) registration: ExposedOutputPortReaderRegistration,
    pub(crate) processor_id: ProcessorUniqueId,
    pub(crate) port_cast: String,
}

/// Register a reader from `location` against output port `port_name` of node
/// `node_name` on `graph`, the graph of the stream `stream_name`, as
/// [`LoadedStreamInThisRuntime::register_a_reader_of_an_exposed_output_port`]
/// does, for a caller already holding that graph's lock. Hands back, beside
/// the outcome, the readers the caller drops once it has let go of the lock.
pub(crate) fn register_a_reader_on_this_graph(
    graph: &mut Graph,
    stream_name: &str,
    node_name: &str,
    port_name: &str,
    location: OutputPortReaderOutsideItsStream,
    cut_off: CutOffAReaderOfAnExposedOutputPort,
) -> (
    Result<ReaderRegisteredOnAGraph>,
    Vec<ReaderOfAnExposedOutputPort>,
) {
    let still_registered = Arc::new(AtomicBool::new(true));
    let reader = ReaderOfAnExposedOutputPort::new(Arc::clone(&still_registered), location, cut_off);
    let (node, port_cast) =
        match the_output_port_named_on_this_graph(graph, stream_name, node_name, port_name) {
            Ok(found) => found,
            Err(refusal) => return (Err(refusal), vec![reader]),
        };
    let outcome = match node.get_mut::<ExposedOutputPortsComponent>() {
        Some(exposed_ports) => exposed_ports.register_reader(&port_cast, reader),
        None => Err((reader, OutputPortExposureLevel::Internal)),
    };
    match outcome {
        Ok(registration_dropped) => (
            Ok(ReaderRegisteredOnAGraph {
                registration: ExposedOutputPortReaderRegistration { still_registered },
                processor_id: node.id.clone(),
                port_cast,
            }),
            registration_dropped,
        ),
        Err((refused_reader, level)) => (
            Err(a_reader_the_port_does_not_allow(
                stream_name,
                &node.display_name,
                port_cast,
                level,
                location,
            )),
            vec![refused_reader],
        ),
    }
}

/// Refuse a reader from `location` of output port `port_name` of node
/// `node_name` on `graph`, the graph of the stream `stream_name`, exactly as
/// [`register_a_reader_on_this_graph`] would, leaving no reader behind.
pub(crate) fn refuse_a_reader_the_port_does_not_allow(
    graph: &mut Graph,
    stream_name: &str,
    node_name: &str,
    port_name: &str,
    location: OutputPortReaderOutsideItsStream,
) -> Result<()> {
    let (node, port_cast) =
        the_output_port_named_on_this_graph(graph, stream_name, node_name, port_name)?;
    let level = node
        .get::<ExposedOutputPortsComponent>()
        .map_or(OutputPortExposureLevel::Internal, |exposed_ports| {
            exposed_ports.level_of(&port_cast)
        });
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
fn the_output_port_named_on_this_graph<'graph>(
    graph: &'graph mut Graph,
    stream_name: &str,
    node_name: &str,
    port_name: &str,
) -> Result<(&'graph mut ProcessorNode, String)> {
    let Some(named) = graph.traversal().v_with_node_name(node_name).first() else {
        return Err(Error::ProcessorNotFound(format!(
            "no node in the stream `{stream_name}` is named {node_name:?}. The stream \
             `{stream_name}` holds: {}",
            node_names_listed_for_a_refusal(
                graph
                    .traversal()
                    .v(())
                    .iter()
                    .map(|node| node.display_name.as_str())
            )
        )));
    };
    let port_cast = cast_exposed_name_to_url_safe(port_name)?.into_owned();
    if !named.has_output(&port_cast) {
        return Err(Error::GraphError(format!(
            "node `{}` in stream `{stream_name}` has no output port `{port_name}`. Its output \
             ports are: {}",
            named.display_name,
            port_names_listed_for_a_refusal(
                named
                    .ports
                    .outputs
                    .iter()
                    .map(|output| output.name.as_str())
            )
        )));
    }
    let processor_id = named.id.clone();
    graph
        .traversal_mut()
        .v(&processor_id)
        .first_mut()
        .map(|node| (node, port_cast))
        .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))
}
