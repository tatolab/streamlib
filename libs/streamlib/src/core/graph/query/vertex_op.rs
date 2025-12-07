mod private {
    use crate::core::graph::ProcessorId;

    pub trait IntoVertexFilter {
        fn into_filter(self) -> Option<ProcessorId>;
    }

    impl IntoVertexFilter for () {
        fn into_filter(self) -> Option<ProcessorId> {
            None
        }
    }

    impl IntoVertexFilter for ProcessorId {
        fn into_filter(self) -> Option<ProcessorId> {
            Some(self)
        }
    }

    impl IntoVertexFilter for &ProcessorId {
        fn into_filter(self) -> Option<ProcessorId> {
            Some(self.clone())
        }
    }

    impl IntoVertexFilter for &str {
        fn into_filter(self) -> Option<ProcessorId> {
            Some(ProcessorId::from(self))
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
    /// - `ProcessorId` - vertex by ID
    pub fn V(self, filter: impl private::IntoVertexFilter) -> ProcessorQuery<'a> {
        match filter.into_filter() {
            Some(id) => {
                // code for some
                self.graph
                    .internal_graph()
                    .graph()
                    .node_references()
                    .find(|(_, processor_node)| processor_node.id == id)
                    .map(|(_, processor_node)| ProcessorQuery {
                        graph: self.graph,
                        ids: vec![processor_node.id.clone()],
                    })
                    .unwrap_or_else(|| ProcessorQuery {
                        graph: self.graph,
                        ids: vec![],
                    })
            }
            None => {
                let ids = self
                    .graph
                    .internal_graph()
                    .graph()
                    .node_references()
                    .map(|(_, processor_node)| processor_node.id.clone())
                    .collect::<Vec<_>>();
                ProcessorQuery {
                    graph: self.graph,
                    ids,
                }
            }
        }
    }
}
