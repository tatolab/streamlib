// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Error, Result};
use crate::core::graph::{Link, ProcessorNode};
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeFiltered;

/// Validate graph structure
pub fn validate_graph(graph: &DiGraph<ProcessorNode, Link>) -> Result<()> {
    // A link reading another stream is an edge from its target's node to
    // itself, which is no cycle between this stream's nodes.
    let links_between_this_streams_nodes = EdgeFiltered::from_fn(graph, |edge| {
        edge.weight().source_in_this_stream().is_some()
    });
    if is_cyclic_directed(&links_between_this_streams_nodes) {
        return Err(Error::InvalidGraph("Graph contains cycles".into()));
    }

    // Future validation:
    // - Check port types match
    // - Check all connections reference valid ports
    // - etc.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::{Graph, InputLinkPortRef, OutputPortInAnotherStreamOnThisMachine};
    use crate::core::processors::ProcessorSpec;
    use crate::core::test_support::{MockProcessor, ensure_test_mocks_registered};

    fn a_node_reading_and_writing(graph: &mut Graph) -> String {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                MockProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("the node is added")
            .id
            .to_string()
    }

    /// The self-loop a link reading another stream is stored as is no cycle,
    /// while a loop of this stream's own nodes still is.
    #[test]
    fn a_link_reading_another_stream_is_no_cycle_and_a_loop_in_the_stream_still_is() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let node = a_node_reading_and_writing(&mut graph);
        assert!(
            graph
                .traversal_mut()
                .add_e_reading_another_stream(
                    OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "video").unwrap(),
                    InputLinkPortRef::new(node.as_str(), "in1"),
                )
                .exists()
        );
        validate_graph(graph.petgraph_digraph_for_a_test())
            .expect("a link reading another stream is no cycle");

        assert!(
            graph
                .traversal_mut()
                .add_e(
                    crate::core::graph::OutputLinkPortRef::new(node.as_str(), "out1"),
                    InputLinkPortRef::new(node.as_str(), "in1"),
                )
                .exists()
        );
        assert!(matches!(
            validate_graph(graph.petgraph_digraph_for_a_test()),
            Err(Error::InvalidGraph(_))
        ));
    }
}
