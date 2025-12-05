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
    fn query_downstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId>;

    /// Get IDs of processors upstream of the given processor (incoming links).
    fn query_upstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId>;

    /// Get IDs of outgoing links from a processor.
    fn query_outgoing_link_ids(&self, id: &ProcessorId) -> Vec<LinkId>;

    /// Get IDs of incoming links to a processor.
    fn query_incoming_link_ids(&self, id: &ProcessorId) -> Vec<LinkId>;

    /// Get all link IDs in the graph.
    fn query_all_link_ids(&self) -> Vec<LinkId>;
}

impl InternalProcessorLinkGraphQueryOperations for InternalProcessorLinkGraph {
    fn query_downstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .neighbors_directed(idx, Direction::Outgoing)
                    .map(|n| self.graph()[n].id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_upstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .neighbors_directed(idx, Direction::Incoming)
                    .map(|n| self.graph()[n].id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_outgoing_link_ids(&self, id: &ProcessorId) -> Vec<LinkId> {
        self.processor_to_node_index(id)
            .map(|idx| {
                self.graph()
                    .edges_directed(idx, Direction::Outgoing)
                    .map(|e| e.weight().id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_incoming_link_ids(&self, id: &ProcessorId) -> Vec<LinkId> {
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
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("middle".into(), "MiddleProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        graph.add_link_by_address("source.output".into(), "middle.input".into());
        graph.add_link_by_address("middle.output".into(), "sink.input".into());

        let downstream = graph.query_downstream_processor_ids(&"source".into());
        assert_eq!(downstream, vec!["middle".to_string()]);

        let downstream = graph.query_downstream_processor_ids(&"middle".into());
        assert_eq!(downstream, vec!["sink".to_string()]);

        let downstream = graph.query_downstream_processor_ids(&"sink".into());
        assert!(downstream.is_empty());
    }

    #[test]
    fn test_upstream_processor_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        graph.add_link_by_address("source.output".into(), "sink.input".into());

        let upstream = graph.query_upstream_processor_ids(&"sink".into());
        assert_eq!(upstream, vec!["source".to_string()]);

        let upstream = graph.query_upstream_processor_ids(&"source".into());
        assert!(upstream.is_empty());
    }

    #[test]
    fn test_outgoing_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        graph.add_processor("source".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink1".into(), "SinkProcessor".into(), 0);
        graph.add_processor("sink2".into(), "SinkProcessor".into(), 0);

        let link1 = graph.add_link_by_address("source.output".into(), "sink1.input".into());
        let link2 = graph.add_link_by_address("source.output".into(), "sink2.input".into());

        let outgoing = graph.query_outgoing_link_ids(&"source".into());
        assert_eq!(outgoing.len(), 2);
        assert!(outgoing.contains(&link1));
        assert!(outgoing.contains(&link2));
    }

    #[test]
    fn test_incoming_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        graph.add_processor("source1".into(), "SourceProcessor".into(), 0);
        graph.add_processor("source2".into(), "SourceProcessor".into(), 0);
        graph.add_processor("sink".into(), "SinkProcessor".into(), 0);

        let link1 = graph.add_link_by_address("source1.output".into(), "sink.input".into());
        let link2 = graph.add_link_by_address("source2.output".into(), "sink.input".into());

        let incoming = graph.query_incoming_link_ids(&"sink".into());
        assert_eq!(incoming.len(), 2);
        assert!(incoming.contains(&link1));
        assert!(incoming.contains(&link2));
    }

    #[test]
    fn test_all_link_ids() {
        let mut graph = InternalProcessorLinkGraph::new();
        graph.add_processor("a".into(), "Processor".into(), 0);
        graph.add_processor("b".into(), "Processor".into(), 0);
        graph.add_processor("c".into(), "Processor".into(), 0);

        let link1 = graph.add_link_by_address("a.output".into(), "b.input".into());
        let link2 = graph.add_link_by_address("b.output".into(), "c.input".into());

        let all_links = graph.query_all_link_ids();
        assert_eq!(all_links.len(), 2);
        assert!(all_links.contains(&link1));
        assert!(all_links.contains(&link2));
    }
}
