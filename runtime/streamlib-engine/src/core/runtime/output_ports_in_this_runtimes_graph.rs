// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How the mesh reads this runtime's own graph.
//!
//! The mesh joins in `Runner::new()` before the graph and the iceoryx2 node
//! exist, so it never holds either: it holds this, which the runtime records
//! once it has both. Every read is taken at the moment it is asked — a graph
//! changes while it runs, and an answer computed once would be an answer about
//! what used to be there.

use std::sync::Arc;

use crate::core::compiler::Compiler;
use crate::core::graph::{Graph, OutputLinkPortRef};
use crate::core::runtime::mesh::{
    HowToReadAnOfferedOutputPort, OutputPortOfferedOnTheMesh, OutputPortsOfferedOnTheMesh,
    WhatThisRuntimeOffersOnTheMesh,
};
use crate::iceoryx2::Iceoryx2Node;

/// This runtime's graph, as the mesh reads it.
pub(crate) struct OutputPortsInThisRuntimesGraph {
    compiler: Arc<Compiler>,
    iceoryx2_node: Iceoryx2Node,
}

impl OutputPortsInThisRuntimesGraph {
    /// Read `compiler`'s graph, sizing channels against `iceoryx2_node`.
    pub(crate) fn of(compiler: &Arc<Compiler>, iceoryx2_node: &Iceoryx2Node) -> Arc<Self> {
        Arc::new(Self {
            compiler: Arc::clone(compiler),
            iceoryx2_node: iceoryx2_node.clone(),
        })
    }
}

impl WhatThisRuntimeOffersOnTheMesh for OutputPortsInThisRuntimesGraph {
    fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh {
        self.compiler
            .scope(|graph, _tx| OutputPortsOfferedOnTheMesh {
                ports: every_output_port_in(graph),
            })
    }

    fn how_to_read_an_offered_output_port(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<HowToReadAnOfferedOutputPort> {
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v_with_display_name(processor_display_name)
                .first()?;
            if !node.has_output(port_name) {
                return None;
            }
            let source_processor_id = node.id.clone();
            // Said rather than silently skipped: every graph output is offered,
            // so one whose name the channel grammar cannot carry reaches here,
            // and the egress table can only report that the link waits on an
            // egress that never starts. This is the one place that knows why.
            let channel_service_name =
                match crate::iceoryx2::source_channel_name(source_processor_id.as_str(), port_name)
                {
                    Ok(channel_service_name) => channel_service_name.into_string(),
                    Err(cannot_be_named) => {
                        tracing::warn!(
                            "{processor_display_name}/{port_name} is offered on the mesh and                              cannot be sent: its channel cannot be named: {cannot_be_named}"
                        );
                        return None;
                    }
                };
            let source = OutputLinkPortRef::new(source_processor_id.clone(), port_name);
            // A port nothing here reads has no channel and no publisher — the
            // first `connect` out of it is what makes both, and across the mesh
            // there is no `connect`. The egress is that port's first consumer,
            // so this is where its channel comes from.
            let channel = match crate::core::compiler::compiler_ops::open_the_channel_of_an_output_port_nothing_local_reads(
                graph,
                &self.iceoryx2_node,
                &source,
            ) {
                Ok(channel) => channel,
                Err(cannot_open) => {
                    tracing::warn!(
                        "{processor_display_name}/{port_name} is offered on the mesh and cannot \
                         be sent: {cannot_open}"
                    );
                    return None;
                }
            };
            Some(HowToReadAnOfferedOutputPort {
                channel_service_name,
                // The sizing the channel was opened with: the egress reopens
                // that service and takes a destination slot on it, so it asks
                // for exactly what is there — or, for a helper's port, for
                // exactly what the helper was told to create it with.
                channel_sizing: channel.channel_sizing,
                the_helpers_answer_that_it_opened_its_publisher: channel
                    .the_helpers_answer_that_it_opened_its_publisher,
            })
        })
    }
}

/// Every output port of every processor in `graph`, addressed the way the mesh
/// addresses one.
fn every_output_port_in(graph: &Graph) -> Vec<OutputPortOfferedOnTheMesh> {
    let mut offered: Vec<OutputPortOfferedOnTheMesh> = graph
        .traversal()
        .v(())
        .iter()
        .flat_map(|node| {
            node.ports
                .outputs
                .iter()
                .map(move |port| OutputPortOfferedOnTheMesh {
                    processor_display_name: node.display_name.clone(),
                    port_name: port.name.clone(),
                })
        })
        .collect();
    // Sorted so two runs of one graph answer the same, and so a refusal that
    // lists them reads the same on every machine.
    offered.sort();
    offered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::processors::{ProcessorInstance, ProcessorSpec};
    use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

    /// A compiler holding one app-process output-only mock, with its instance
    /// attached the way the compiler's spawn phase attaches one — and its
    /// display name, which is how the mesh addresses it.
    fn a_compiler_holding_one_output_only_processor() -> (
        Arc<Compiler>,
        String,
        Arc<crate::iceoryx2::OutputWriterInner>,
    ) {
        ensure_test_mocks_registered();
        let compiler = Arc::new(Compiler::new());
        let (display_name, output_writer) = compiler.scope(|graph, _tx| {
            let node = graph
                .traversal_mut()
                .add_v(ProcessorSpec::new(
                    MockOutputOnlyProcessor::Processor::processor_class_import_path(),
                    serde_json::Value::Null,
                ))
                .first()
                .expect("the mock is in the registry");
            let (processor_id, display_name) = (node.id.to_string(), node.display_name.clone());

            let mut instance = ProcessorInstance::new(Box::new(
                <MockOutputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                    Default::default(),
                )
                .expect("the mock constructs from its default config"),
            ));
            instance
                .install_iceoryx2_resources()
                .expect("the mock accepts its iceoryx2 resources");
            let output_writer = instance
                .iceoryx2_output_writer_inner()
                .expect("an output-only mock has an output writer");
            ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(
                graph
                    .traversal_mut()
                    .v(processor_id.as_str())
                    .first_mut()
                    .expect("the node was just added"),
            );
            (display_name, output_writer)
        });
        (compiler, display_name, output_writer)
    }

    /// Asking how to read an offered port is what opens its channel, because
    /// the mesh's egress is the first consumer a port nothing local reads ever
    /// has.
    ///
    /// Mental-revert: take the opener out of `how_to_read_an_offered_output_port`
    /// and this goes red. Nothing else in CI drives that call site — the
    /// two-process proof stands the mesh half up with no compiler, and the rig
    /// arm is rig-only, which is exactly how the hole reached the rig.
    #[test]
    fn asking_how_to_read_an_offered_port_is_what_opens_its_channel() {
        let (compiler, display_name, output_writer) =
            a_compiler_holding_one_output_only_processor();
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let reads_the_graph = OutputPortsInThisRuntimesGraph::of(&compiler, &node);

        assert!(
            !output_writer.has_channel_publisher("out1"),
            "nothing has connected to this port, so it has no publisher yet"
        );

        let how_to_read = reads_the_graph
            .how_to_read_an_offered_output_port(&display_name, "out1")
            .expect("an offered port says how to read it");

        assert!(
            output_writer.has_channel_publisher("out1"),
            "asking how to read the port must have opened its channel, or the port publishes \
             nothing and the reader's link reads wired over silence"
        );
        assert!(
            how_to_read.channel_service_name.ends_with("/out1"),
            "the egress is told the port's own channel; got {}",
            how_to_read.channel_service_name
        );
    }

    /// Every output port in the graph is offered, under its processor's display
    /// name — the name a peer addresses it by — and asking does not open
    /// anything, because a runtime does no work for a port nobody reads.
    #[test]
    fn every_output_port_is_offered_under_its_display_name_and_listing_opens_nothing() {
        let (compiler, display_name, output_writer) =
            a_compiler_holding_one_output_only_processor();
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let reads_the_graph = OutputPortsInThisRuntimesGraph::of(&compiler, &node);

        let offered = reads_the_graph.output_ports_it_offers_right_now();
        assert!(offered.offers(&display_name, "out1"), "{offered:?}");
        assert!(
            !output_writer.has_channel_publisher("out1"),
            "listing what is offered must open no channel: a sending runtime does no work for a \
             port nobody reads"
        );
    }

    /// A port no processor here has says so by answering nothing, which is what
    /// makes the reader's refusal name what *is* offered.
    #[test]
    fn a_port_this_runtime_does_not_have_says_how_to_read_nothing() {
        let (compiler, display_name, _) = a_compiler_holding_one_output_only_processor();
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let reads_the_graph = OutputPortsInThisRuntimesGraph::of(&compiler, &node);

        assert!(
            reads_the_graph
                .how_to_read_an_offered_output_port(&display_name, "no_such_port")
                .is_none()
        );
        assert!(
            reads_the_graph
                .how_to_read_an_offered_output_port("NoSuchProcessor", "out1")
                .is_none()
        );
    }
}
