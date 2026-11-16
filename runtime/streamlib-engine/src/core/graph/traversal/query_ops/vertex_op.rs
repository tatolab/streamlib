// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod private {
    use crate::core::graph::NodeRunId;

    pub trait IntoVertexFilter {
        fn into_filter(self) -> Option<NodeRunId>;
    }

    impl IntoVertexFilter for () {
        fn into_filter(self) -> Option<NodeRunId> {
            None
        }
    }

    impl IntoVertexFilter for NodeRunId {
        fn into_filter(self) -> Option<NodeRunId> {
            Some(self)
        }
    }

    impl IntoVertexFilter for &NodeRunId {
        fn into_filter(self) -> Option<NodeRunId> {
            Some(self.clone())
        }
    }

    impl IntoVertexFilter for &str {
        fn into_filter(self) -> Option<NodeRunId> {
            Some(NodeRunId::from(self))
        }
    }
}

use petgraph::visit::IntoNodeReferences;

use crate::core::graph::{NodeTraversal, NodeTraversalMut, TraversalSource, TraversalSourceMut};

impl<'a> TraversalSource<'a> {
    pub fn v(self, filter: impl private::IntoVertexFilter) -> NodeTraversal<'a> {
        let ids = match filter.into_filter() {
            Some(id) => self
                .graph
                .node_references()
                .find(|(_, graph_node)| graph_node.id == id)
                .map(|(idx, _)| vec![idx])
                .unwrap_or_default(),
            None => self
                .graph
                .node_references()
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>(),
        };
        NodeTraversal {
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
    pub fn v_with_node_name(self, node_name: &str) -> NodeTraversal<'a> {
        let ids = crate::core::graph::cast_exposed_name_to_url_safe(node_name)
            .ok()
            .and_then(|cast| {
                self.graph
                    .node_references()
                    .find(|(_, graph_node)| graph_node.display_name == cast)
            })
            .map(|(idx, _)| vec![idx])
            .unwrap_or_default();
        NodeTraversal {
            graph: self.graph,
            ids,
        }
    }
}

impl<'a> TraversalSourceMut<'a> {
    pub fn v(self, filter: impl private::IntoVertexFilter) -> NodeTraversalMut<'a> {
        let ids = match filter.into_filter() {
            Some(id) => self
                .graph
                .node_references()
                .find(|(_, graph_node)| graph_node.id == id)
                .map(|(idx, _)| vec![idx])
                .unwrap_or_default(),
            None => self
                .graph
                .node_references()
                .map(|(idx, _)| idx)
                .collect::<Vec<_>>(),
        };
        NodeTraversalMut {
            graph: self.graph,
            ids,
        }
    }
}
