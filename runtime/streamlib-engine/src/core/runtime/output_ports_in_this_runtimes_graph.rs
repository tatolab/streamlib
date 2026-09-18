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
            let channel_service_name =
                crate::iceoryx2::source_channel_name(source_processor_id.as_str(), port_name)
                    .ok()?
                    .into_string();
            // The sizing the compiler opened the channel with: an egress
            // reopens that service and takes a destination slot on it, so it
            // asks for exactly what is already there.
            let channel_sizing = crate::core::compiler::compiler_ops::resolve_channel_sizing(
                graph,
                &self.iceoryx2_node,
                &OutputLinkPortRef::new(source_processor_id.clone(), port_name),
            )
            .ok()?;
            Some(HowToReadAnOfferedOutputPort {
                channel_service_name,
                channel_sizing,
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
                .map(|port| OutputPortOfferedOnTheMesh {
                    processor_display_name: node.display_name.clone(),
                    port_name: port.name.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // Sorted so two runs of one graph answer the same, and so a refusal that
    // lists them reads the same on every machine.
    offered.sort();
    offered
}
