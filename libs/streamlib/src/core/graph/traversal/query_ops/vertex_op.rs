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
        match filter.into_filter() {
            Some(id) => {
                // code for some
                self.graph
                    .node_references()
                    .find(|(_, processor_node)| processor_node.id == id)
                    .map(|(idx, _)| ProcessorTraversal {
                        graph: self.graph,
                        ids: vec![idx],
                    })
                    .unwrap_or_else(|| ProcessorTraversal {
                        graph: self.graph,
                        ids: vec![],
                    })
            }
            None => {
                let ids = self
                    .graph
                    .node_references()
                    .map(|(idx, _)| idx)
                    .collect::<Vec<_>>();
                ProcessorTraversal {
                    graph: self.graph,
                    ids,
                }
            }
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
        match filter.into_filter() {
            Some(id) => {
                let found = self
                    .graph
                    .node_references()
                    .find(|(_, processor_node)| processor_node.id == id)
                    .map(|(idx, _)| idx);
                ProcessorTraversalMut {
                    graph: self.graph,
                    ids: found.map(|idx| vec![idx]).unwrap_or_default(),
                }
            }
            None => {
                let ids = self
                    .graph
                    .node_references()
                    .map(|(idx, _)| idx)
                    .collect::<Vec<_>>();
                ProcessorTraversalMut {
                    graph: self.graph,
                    ids,
                }
            }
        }
    }
}
