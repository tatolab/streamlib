// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod private {
    use crate::core::graph::LinkUniqueId;

    pub trait IntoEdgeFilter {
        fn into_filter(self) -> Option<LinkUniqueId>;
    }

    impl IntoEdgeFilter for () {
        fn into_filter(self) -> Option<LinkUniqueId> {
            None
        }
    }

    impl IntoEdgeFilter for LinkUniqueId {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(self)
        }
    }

    impl IntoEdgeFilter for &LinkUniqueId {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(self.clone())
        }
    }

    impl IntoEdgeFilter for &str {
        fn into_filter(self) -> Option<LinkUniqueId> {
            Some(LinkUniqueId::from(self))
        }
    }
}

use petgraph::visit::EdgeRef;

use crate::core::{graph::TraversalSource, LinkTraversal};
impl<'a> TraversalSource<'a> {
    /// Start traversal from edges.
    ///
    /// Accepts:
    /// - `()` - all edges
    /// - `&str` - edge by ID string
    /// - `LinkUniqueId` - edge by ID
    pub fn e(self, filter: impl private::IntoEdgeFilter) -> LinkTraversal<'a> {
        match filter.into_filter() {
            Some(id) => {
                // code for some
                self.graph
                    .edge_references()
                    .find(|edge_ref| edge_ref.weight().id == id)
                    .map(|edge_ref| LinkTraversal {
                        graph: self.graph,
                        ids: vec![edge_ref.id()],
                    })
                    .unwrap_or_else(|| LinkTraversal {
                        graph: self.graph,
                        ids: vec![],
                    })
            }
            None => {
                let ids = self
                    .graph
                    .edge_references()
                    .map(|edge_ref| edge_ref.id())
                    .collect::<Vec<_>>();
                LinkTraversal {
                    graph: self.graph,
                    ids,
                }
            }
        }
    }
}
