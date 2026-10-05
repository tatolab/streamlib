// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod private {
    use crate::core::graph::ProcessorUniqueId;

    pub trait IntoVertexFilter {
        fn into_filter(self) -> Option<ProcessorUniqueId>;
    }

    impl IntoVertexFilter for () {
        fn into_filter(self) -> Option<ProcessorUniqueId> {
            None
        }
    }

    impl IntoVertexFilter for ProcessorUniqueId {
        fn into_filter(self) -> Option<ProcessorUniqueId> {
            Some(self)
        }
    }

    impl IntoVertexFilter for &ProcessorUniqueId {
        fn into_filter(self) -> Option<ProcessorUniqueId> {
            Some(self.clone())
        }
    }

    impl IntoVertexFilter for &str {
        fn into_filter(self) -> Option<ProcessorUniqueId> {
            Some(ProcessorUniqueId::from(self))
        }
    }
}

use petgraph::visit::IntoNodeReferences;

use crate::core::graph::{
    ProcessorTraversal, ProcessorTraversalMut, TraversalSource, TraversalSourceMut,
};

impl<'a> TraversalSource<'a> {
    /// Start traversal from vertices.
    ///
    /// Accepts:
    /// - `()` - all vertices
    /// - `&str` - vertex by ID string
    /// - `ProcessorUniqueId` - vertex by ID
    pub fn v(self, filter: impl private::IntoVertexFilter) -> ProcessorTraversal<'a> {
        let ids = match filter.into_filter() {
            Some(id) => self
                .graph
                .node_references()
                .find(|(_, processor_node)| processor_node.id == id)
                .map(|(idx, _)| vec![idx])
                .unwrap_or_default(),
            None => self
                .graph
                .node_references()
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>(),
        };
        ProcessorTraversal {
            graph: self.graph,
            ids,
        }
    }

    /// Start traversal from the node `node_name` names once cast — empty when
    /// no node has that name, or it casts to nothing.
    ///
    /// A node's name is unique within a graph and is its part of a port's
    /// address — so this is how an address is turned back into one of this
    /// runtime's own nodes.
    pub fn v_with_node_name(self, node_name: &str) -> ProcessorTraversal<'a> {
        let ids = crate::core::graph::cast_exposed_name_to_url_safe(node_name)
            .ok()
            .and_then(|cast| {
                self.graph
                    .node_references()
                    .find(|(_, processor_node)| processor_node.display_name == cast)
            })
            .map(|(idx, _)| vec![idx])
            .unwrap_or_default();
        ProcessorTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> TraversalSourceMut<'a> {
    /// Start traversal from vertices.
    ///
    /// Accepts:
    /// - `()` - all vertices
    /// - `&str` - vertex by ID string
    /// - `ProcessorUniqueId` - vertex by ID
    pub fn v(self, filter: impl private::IntoVertexFilter) -> ProcessorTraversalMut<'a> {
        let ids = match filter.into_filter() {
            Some(id) => self
                .graph
                .node_references()
                .find(|(_, processor_node)| processor_node.id == id)
                .map(|(idx, _)| vec![idx])
                .unwrap_or_default(),
            None => self
                .graph
                .node_references()
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>(),
        };
        ProcessorTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
