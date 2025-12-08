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

use crate::core::graph::{ProcessorQuery, QueryBuilder};

impl<'a> QueryBuilder<'a> {
    /// Start traversal from vertices.
    ///
    /// Accepts:
    /// - `()` - all vertices
    /// - `&str` - vertex by ID string
    /// - `ProcessorUniqueId` - vertex by ID
    pub fn v(self, filter: impl private::IntoVertexFilter) -> ProcessorQuery<'a> {
        match filter.into_filter() {
            Some(id) => {
                // code for some
                self.graph
                    .node_references()
                    .find(|(_, processor_node)| processor_node.id == id)
                    .map(|(idx, _)| ProcessorQuery {
                        graph: self.graph,
                        ids: vec![idx],
                    })
                    .unwrap_or_else(|| ProcessorQuery {
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
                ProcessorQuery {
                    graph: self.graph,
                    ids,
                }
            }
        }
    }
}
