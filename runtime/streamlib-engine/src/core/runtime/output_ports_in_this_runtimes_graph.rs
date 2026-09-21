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
    HowToReadAnOfferedOutputPort, OutputPortOfferedOnTheMesh,
    OutputPortThisRuntimeHoldsAndCannotSend, OutputPortsOfferedOnTheMesh,
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
            .scope(|graph, _tx| every_output_port_in(graph))
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
            // The same check the offer answered with, so a port this runtime
            // said it cannot send and one refused here can never disagree. A
            // reader of this engine version is refused at the offer and never
            // reaches here; one that raced a graph change does, and is told.
            let channel_service_name = match the_channel_an_output_port_publishes_to(
                source_processor_id.as_str(),
                port_name,
            ) {
                Ok(channel_service_name) => channel_service_name.into_string(),
                Err(why_it_cannot_be_sent) => {
                    tracing::warn!(
                        "{processor_display_name}/{port_name} is being read across the mesh and \
                         cannot be sent: {why_it_cannot_be_sent}"
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

/// The channel the output port `port_name` of the processor `source_processor_id`
/// publishes to, or the reason the mesh cannot send that port.
///
/// The one check the offer and the egress both read, so a runtime can never
/// answer that it offers a port it then declines to send.
///
/// Nameability alone: the offer is answered for every port in the graph on every
/// query, and a sending runtime does no work for a port nobody reads, so this
/// may touch nothing but the two names. Every other way a port turns out
/// unsendable is found when its egress starts, by the egress.
fn the_channel_an_output_port_publishes_to(
    source_processor_id: &str,
    port_name: &str,
) -> std::result::Result<crate::iceoryx2::ChannelName, String> {
    crate::iceoryx2::source_channel_name(source_processor_id, port_name)
        .map_err(|cannot_be_named| format!("its channel cannot be named: {cannot_be_named}"))
}

/// Every output port of every processor in `graph`, addressed the way the mesh
/// addresses one, split into what this runtime can send and what it holds and
/// cannot.
fn every_output_port_in(graph: &Graph) -> OutputPortsOfferedOnTheMesh {
    let mut ports = Vec::new();
    let mut ports_it_holds_and_cannot_send = Vec::new();
    for node in graph.traversal().v(()).iter() {
        for port in &node.ports.outputs {
            match the_channel_an_output_port_publishes_to(node.id.as_str(), &port.name) {
                Ok(_) => ports.push(OutputPortOfferedOnTheMesh {
                    processor_display_name: node.display_name.clone(),
                    port_name: port.name.clone(),
                }),
                Err(why_it_cannot_be_sent) => {
                    ports_it_holds_and_cannot_send.push(OutputPortThisRuntimeHoldsAndCannotSend {
                        processor_display_name: node.display_name.clone(),
                        port_name: port.name.clone(),
                        why_it_cannot_be_sent,
                    })
                }
            }
        }
    }
    // Sorted so two runs of one graph answer the same, and so a refusal that
    // lists them reads the same on every machine.
    ports.sort();
    ports_it_holds_and_cannot_send.sort();
    // Spelled out rather than built by mutating a default: this is the one place
    // the document is produced, so a field added to it must fail here rather
    // than reach every peer as whatever `Default` gives.
    OutputPortsOfferedOnTheMesh {
        ports,
        ports_it_holds_and_cannot_send,
        // Empty from the graph, always: a graph holds no egresses, so why one
        // ended is the registry's to add to this answer.
        ports_it_stopped_sending: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::processors::{ProcessorInstance, ProcessorSpec};
    use crate::core::test_support::{
        MockOutputOnlyProcessor, MockProcessorWhoseOutputPortTheChannelGrammarCannotName,
        ensure_test_mocks_registered,
    };

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

    /// A port whose channel the grammar cannot name is not offered — it is
    /// answered as one this runtime holds and cannot send, with the reason, so
    /// the reader refuses at once instead of waiting on an egress that can never
    /// start.
    ///
    /// Mental-revert: put every graph output back into `ports` and this goes red
    /// on both halves — and the reader's link goes back to `awaiting_remote` on
    /// an egress that can never start, which is what reached the rig.
    #[test]
    fn a_port_whose_channel_cannot_be_named_is_held_and_unsendable_rather_than_offered() {
        ensure_test_mocks_registered();
        let compiler = Arc::new(Compiler::new());
        let display_name = compiler.scope(|graph, _tx| {
            graph
                .traversal_mut()
                .add_v(ProcessorSpec::new(
                    MockProcessorWhoseOutputPortTheChannelGrammarCannotName::Processor::processor_class_import_path(),
                    serde_json::Value::Null,
                ))
                .first()
                .expect("the mock is in the registry")
                .display_name
                .clone()
        });
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let reads_the_graph = OutputPortsInThisRuntimesGraph::of(&compiler, &node);

        let answered = reads_the_graph.output_ports_it_offers_right_now();
        assert!(
            !answered.offers(&display_name, "outOne"),
            "a port the mesh cannot send is not on offer: {answered:?}"
        );
        let why = answered
            .why_it_cannot_send(&display_name, "outOne")
            .expect("the port it holds and cannot send is answered with its reason");
        assert!(why.contains("channel cannot be named"), "{why}");
        assert!(why.contains('O'), "the reason names the character: {why}");
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
