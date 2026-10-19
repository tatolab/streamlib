// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Error, Result};
use crate::core::graph::{Link, ProcessorNode};
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::DiGraph;

/// Refuse a graph whose links loop back through its own nodes.
pub fn validate_graph(graph: &DiGraph<ProcessorNode, Link>) -> Result<()> {
    if is_cyclic_directed(graph) {
        return Err(Error::InvalidGraph("Graph contains cycles".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::{Graph, InputLinkPortRef};
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

    #[test]
    fn a_link_looping_back_into_its_own_node_is_a_cycle() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let node = a_node_reading_and_writing(&mut graph);
        validate_graph(graph.petgraph_digraph_for_a_test()).expect("a lone node is no cycle");

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
