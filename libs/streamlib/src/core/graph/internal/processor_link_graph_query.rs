// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Internal query operations for InternalProcessorLinkGraph.
//!
//! Provides traversal primitives used by the query executor.

use petgraph::Direction;

use crate::core::graph::ProcessorId;
use crate::core::links::LinkId;

use super::processor_link_graph::InternalProcessorLinkGraph;

/// Internal trait for graph traversal primitives.
///
/// Implemented by [`InternalProcessorLinkGraph`] to support query execution.
pub(crate) trait InternalProcessorLinkGraphQueryOperations {
    /// Get IDs of processors downstream of the given processor (outgoing links).
    fn query_downstream_processor_ids(&self, id: impl AsRef<str>) -> Vec<ProcessorId>;

    /// Get IDs of processors upstream of the given processor (incoming links).
    fn query_upstream_processor_ids(&self, id: impl AsRef<str>) -> Vec<ProcessorId>;

    /// Get IDs of outgoing links from a processor.
    fn query_outgoing_link_ids(&self, id: impl AsRef<str>) -> Vec<LinkId>;

    /// Get IDs of incoming links to a processor.
    fn query_incoming_link_ids(&self, id: impl AsRef<str>) -> Vec<LinkId>;

    /// Get all link IDs in the graph.
    fn query_all_link_ids(&self) -> Vec<LinkId>;
}

impl InternalProcessorLinkGraphQueryOperations for InternalProcessorLinkGraph {
    fn query_downstream_processor_ids(&self, id: impl AsRef<str>) -> Vec<ProcessorId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .neighbors_directed(idx, Direction::Outgoing)
                    .map(|n| self.graph()[n].id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_upstream_processor_ids(&self, id: impl AsRef<str>) -> Vec<ProcessorId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .neighbors_directed(idx, Direction::Incoming)
                    .map(|n| self.graph()[n].id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_outgoing_link_ids(&self, id: impl AsRef<str>) -> Vec<LinkId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .edges_directed(idx, Direction::Outgoing)
                    .map(|e| e.weight().id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_incoming_link_ids(&self, id: impl AsRef<str>) -> Vec<LinkId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .edges_directed(idx, Direction::Incoming)
                    .map(|e| e.weight().id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_all_link_ids(&self) -> Vec<LinkId> {
        self.links().iter().map(|l| l.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downstream_processor_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let middle = graph.add_processor("MiddleProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", middle));
        graph.add_link_by_address(format!("{}.output", middle), format!("{}.input", sink));

        let downstream = graph.query_downstream_processor_ids(&source);
        assert_eq!(downstream.len(), 1);
        assert_eq!(downstream[0], middle);

        let downstream = graph.query_downstream_processor_ids(&middle);
        assert_eq!(downstream.len(), 1);
        assert_eq!(downstream[0], sink);

        let downstream = graph.query_downstream_processor_ids(&sink);
        assert!(downstream.is_empty());
    }

    #[test]
    fn test_upstream_processor_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink));

        let upstream = graph.query_upstream_processor_ids(&sink);
        assert_eq!(upstream.len(), 1);
        assert_eq!(upstream[0], source);

        let upstream = graph.query_upstream_processor_ids(&source);
        assert!(upstream.is_empty());
    }

    #[test]
    fn test_outgoing_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source = graph.add_processor("SourceProcessor").id.clone();
        let sink1 = graph.add_processor("SinkProcessor").id.clone();
        let sink2 = graph.add_processor("SinkProcessor").id.clone();

        let link1 =
            graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink1));
        let link2 =
            graph.add_link_by_address(format!("{}.output", source), format!("{}.input", sink2));

        let outgoing = graph.query_outgoing_link_ids(&source);
        assert_eq!(outgoing.len(), 2);
        assert!(outgoing.contains(&link1));
        assert!(outgoing.contains(&link2));
    }

    #[test]
    fn test_incoming_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        let source1 = graph.add_processor("SourceProcessor").id.clone();
        let source2 = graph.add_processor("SourceProcessor").id.clone();
        let sink = graph.add_processor("SinkProcessor").id.clone();

        let link1 =
            graph.add_link_by_address(format!("{}.output", source1), format!("{}.input", sink));
        let link2 =
            graph.add_link_by_address(format!("{}.output", source2), format!("{}.input", sink));

        let incoming = graph.query_incoming_link_ids(&sink);
        assert_eq!(incoming.len(), 2);
        assert!(incoming.contains(&link1));
        assert!(incoming.contains(&link2));
    }

    #[test]
    fn test_all_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        let a = graph.add_processor("Processor").id.clone();
        let b = graph.add_processor("Processor").id.clone();
        let c = graph.add_processor("Processor").id.clone();

        let link1 = graph.add_link_by_address(format!("{}.output", a), format!("{}.input", b));
        let link2 = graph.add_link_by_address(format!("{}.output", b), format!("{}.input", c));

        let all_links = graph.query_all_link_ids();
        assert_eq!(all_links.len(), 2);
        assert!(all_links.contains(&link1));
        assert!(all_links.contains(&link2));
    }
}
