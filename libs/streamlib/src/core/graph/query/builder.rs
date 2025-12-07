// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query builder types for graph operations.

use crate::core::graph::traits::{GraphEdge, GraphNode};
use crate::core::graph::{Graph, Link, ProcessorId, ProcessorNode};
use crate::core::links::LinkId;
use crate::core::processors::ProcessorState;

// =============================================================================
// Read Query Builder
// =============================================================================

/// Entry point for read-only graph queries.
pub struct QueryBuilder<'a> {
    pub(super) graph: &'a Graph,
}

impl<'a> QueryBuilder<'a> {
    pub(crate) fn new(graph: &'a Graph) -> Self {
        Self { graph }
    }

    /// Start from all processors (vertices).
    // #[allow(non_snake_case)]
    // pub fn V(self) -> ProcessorQuery<'a> {
    //     let ids = self
    //         .graph
    //         .internal_graph()
    //         .nodes()
    //         .iter()
    //         .map(|n| n.id.clone())
    //         .collect();
    //     ProcessorQuery {
    //         graph: self.graph,
    //         ids,
    //     }
    // }

    /// Start from specific processors by ID.
    #[allow(non_snake_case)]
    pub fn V_from(self, ids: &[ProcessorId]) -> ProcessorQuery<'a> {
        let valid_ids = ids
            .iter()
            .filter(|id| self.graph.internal_graph().has_processor(id))
            .cloned()
            .collect();
        ProcessorQuery {
            graph: self.graph,
            ids: valid_ids,
        }
    }

    /// Start from a single processor by ID.
    #[allow(non_snake_case)]
    pub fn V_id(self, id: impl AsRef<str>) -> ProcessorQuery<'a> {
        let id_ref = id.as_ref();
        let ids = if self.graph.internal_graph().has_processor(id_ref) {
            vec![ProcessorId::from(id_ref)]
        } else {
            vec![]
        };
        ProcessorQuery {
            graph: self.graph,
            ids,
        }
    }

    /// Start from all links (edges).
    #[allow(non_snake_case)]
    pub fn E(self) -> LinkQuery<'a> {
        let ids = self
            .graph
            .internal_graph()
            .links()
            .iter()
            .map(|l| l.id.clone())
            .collect();
        LinkQuery {
            graph: self.graph,
            ids,
        }
    }

    /// Start from specific links by ID.
    #[allow(non_snake_case)]
    pub fn E_from(self, ids: &[LinkId]) -> LinkQuery<'a> {
        let valid_ids = ids
            .iter()
            .filter(|id| self.graph.internal_graph().get_link(id).is_some())
            .cloned()
            .collect();
        LinkQuery {
            graph: self.graph,
            ids: valid_ids,
        }
    }

    /// Start from a single link by ID.
    #[allow(non_snake_case)]
    pub fn E_id(self, id: &LinkId) -> LinkQuery<'a> {
        let ids = if self.graph.internal_graph().get_link(id).is_some() {
            vec![id.clone()]
        } else {
            vec![]
        };
        LinkQuery {
            graph: self.graph,
            ids,
        }
    }
}

// =============================================================================
// Mutable Query Builder
// =============================================================================

/// Entry point for mutable graph queries.
pub struct QueryBuilderMut<'a> {
    graph: &'a mut Graph,
}

impl<'a> QueryBuilderMut<'a> {
    pub(crate) fn new(graph: &'a mut Graph) -> Self {
        Self { graph }
    }

    // /// Start from all processors (vertices).
    // #[allow(non_snake_case)]
    // pub fn V(self) -> ProcessorQueryMut<'a> {
    //     let ids = self
    //         .graph
    //         .internal_graph()
    //         .nodes()
    //         .iter()
    //         .map(|n| n.id.clone())
    //         .collect();
    //     ProcessorQueryMut {
    //         graph: self.graph,
    //         ids,
    //     }
    // }

    /// Start from specific processors by ID.
    #[allow(non_snake_case)]
    pub fn V_from(self, ids: &[ProcessorId]) -> ProcessorQueryMut<'a> {
        let valid_ids = ids
            .iter()
            .filter(|id| self.graph.internal_graph().has_processor(id))
            .cloned()
            .collect();
        ProcessorQueryMut {
            graph: self.graph,
            ids: valid_ids,
        }
    }

    /// Start from a single processor by ID.
    #[allow(non_snake_case)]
    pub fn V_id(self, id: impl AsRef<str>) -> ProcessorQueryMut<'a> {
        let id_ref = id.as_ref();
        let ids = if self.graph.internal_graph().has_processor(id_ref) {
            vec![ProcessorId::from(id_ref)]
        } else {
            vec![]
        };
        ProcessorQueryMut {
            graph: self.graph,
            ids,
        }
    }

    /// Start from all links (edges).
    #[allow(non_snake_case)]
    pub fn E(self) -> LinkQueryMut<'a> {
        let ids = self
            .graph
            .internal_graph()
            .links()
            .iter()
            .map(|l| l.id.clone())
            .collect();
        LinkQueryMut {
            graph: self.graph,
            ids,
        }
    }

    /// Start from specific links by ID.
    #[allow(non_snake_case)]
    pub fn E_from(self, ids: &[LinkId]) -> LinkQueryMut<'a> {
        let valid_ids = ids
            .iter()
            .filter(|id| self.graph.internal_graph().get_link(id).is_some())
            .cloned()
            .collect();
        LinkQueryMut {
            graph: self.graph,
            ids: valid_ids,
        }
    }

    /// Start from a single link by ID.
    #[allow(non_snake_case)]
    pub fn E_id(self, id: &LinkId) -> LinkQueryMut<'a> {
        let ids = if self.graph.internal_graph().get_link(id).is_some() {
            vec![id.clone()]
        } else {
            vec![]
        };
        LinkQueryMut {
            graph: self.graph,
            ids,
        }
    }
}

// =============================================================================
// Processor Query (Read)
// =============================================================================

/// Read-only query over processor nodes.
pub struct ProcessorQuery<'a> {
    pub(super) graph: &'a Graph,
    pub(super) ids: Vec<ProcessorId>,
}

impl<'a> ProcessorQuery<'a> {
    // =========================================================================
    // Filter Operations (weight -> weight)
    // =========================================================================

    /// Filter to processors of a specific type.
    pub fn of_type(mut self, processor_type: &str) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| n.processor_type == processor_type)
                .unwrap_or(false)
        });
        self
    }

    /// Filter to processors in a specific state.
    pub fn in_state(mut self, state: ProcessorState) -> Self {
        use crate::core::graph::components::StateComponent;
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .and_then(|n| n.get::<StateComponent>())
                .map(|sc| *sc.0.lock() == state)
                .unwrap_or(false)
        });
        self
    }

    /// Filter to source processors (no incoming links).
    pub fn sources(mut self) -> Self {
        let sources = self.graph.internal_graph().find_sources();
        self.ids.retain(|id| sources.contains(id));
        self
    }

    /// Filter to sink processors (no outgoing links).
    pub fn sinks(mut self) -> Self {
        let sinks = self.graph.internal_graph().find_sinks();
        self.ids.retain(|id| sinks.contains(id));
        self
    }

    /// Filter with a predicate on processor nodes.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ProcessorNode) -> bool,
    {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| predicate(n))
                .unwrap_or(false)
        });
        self
    }

    /// Filter to processors that have a specific component.
    pub fn has_component<C: Send + Sync + 'static>(mut self) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| n.has::<C>())
                .unwrap_or(false)
        });
        self
    }

    // =========================================================================
    // Traversal Operations (weight -> weight)
    // =========================================================================

    /// Traverse to downstream processors.
    pub fn downstream(self) -> Self {
        let graph = self.graph.internal_graph();
        let new_ids: Vec<_> = self
            .ids
            .iter()
            .flat_map(|id| {
                graph
                    .links()
                    .iter()
                    .filter(|l| l.source.node.as_str() == id.as_str())
                    .map(|l| l.target.node.clone())
            })
            .collect();
        ProcessorQuery {
            graph: self.graph,
            ids: new_ids,
        }
    }

    /// Traverse to upstream processors.
    pub fn upstream(self) -> Self {
        let graph = self.graph.internal_graph();
        let new_ids: Vec<_> = self
            .ids
            .iter()
            .flat_map(|id| {
                graph
                    .links()
                    .iter()
                    .filter(|l| l.target.node.as_str() == id.as_str())
                    .map(|l| l.source.node.clone())
            })
            .collect();
        ProcessorQuery {
            graph: self.graph,
            ids: new_ids,
        }
    }

    /// Get outgoing links from current processors.
    pub fn out_links(self) -> LinkQuery<'a> {
        let graph = self.graph.internal_graph();
        let link_ids: Vec<_> = self
            .ids
            .iter()
            .flat_map(|id| {
                graph
                    .links()
                    .iter()
                    .filter(|l| l.source.node.as_str() == id.as_str())
                    .map(|l| l.id.clone())
            })
            .collect();
        LinkQuery {
            graph: self.graph,
            ids: link_ids,
        }
    }

    /// Get incoming links to current processors.
    pub fn in_links(self) -> LinkQuery<'a> {
        let graph = self.graph.internal_graph();
        let link_ids: Vec<_> = self
            .ids
            .iter()
            .flat_map(|id| {
                graph
                    .links()
                    .iter()
                    .filter(|l| l.target.node.as_str() == id.as_str())
                    .map(|l| l.id.clone())
            })
            .collect();
        LinkQuery {
            graph: self.graph,
            ids: link_ids,
        }
    }

    // =========================================================================
    // Terminal Operations (weight -> data, exits graph)
    // =========================================================================

    /// Return processor IDs.
    pub fn ids(self) -> Vec<ProcessorId> {
        self.ids
    }

    /// Return count of matching processors.
    pub fn count(self) -> usize {
        self.ids.len()
    }

    /// Return first matching processor ID.
    pub fn first_id(self) -> Option<ProcessorId> {
        self.ids.into_iter().next()
    }

    /// Check if any processors match.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }

    /// Get reference to the first matching processor node.
    pub fn first(self) -> Option<&'a ProcessorNode> {
        let id = self.ids.into_iter().next()?;
        self.graph.internal_graph().get_processor(&id)
    }

    /// Iterate over matching processor nodes.
    pub fn iter(self) -> impl Iterator<Item = &'a ProcessorNode> {
        let graph = self.graph;
        self.ids
            .into_iter()
            .filter_map(move |id| graph.internal_graph().get_processor(&id))
    }
}

// =============================================================================
// Processor Query (Mutable)
// =============================================================================

/// Mutable query over processor nodes.
pub struct ProcessorQueryMut<'a> {
    graph: &'a mut Graph,
    ids: Vec<ProcessorId>,
}

impl<'a> ProcessorQueryMut<'a> {
    // =========================================================================
    // Filter Operations (weight -> weight)
    // =========================================================================

    /// Filter to processors of a specific type.
    pub fn of_type(mut self, processor_type: &str) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| n.processor_type == processor_type)
                .unwrap_or(false)
        });
        self
    }

    /// Filter to processors in a specific state.
    pub fn in_state(mut self, state: ProcessorState) -> Self {
        use crate::core::graph::components::StateComponent;
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .and_then(|n| n.get::<StateComponent>())
                .map(|sc| *sc.0.lock() == state)
                .unwrap_or(false)
        });
        self
    }

    /// Filter to source processors (no incoming links).
    pub fn sources(mut self) -> Self {
        let sources = self.graph.internal_graph().find_sources();
        self.ids.retain(|id| sources.contains(id));
        self
    }

    /// Filter to sink processors (no outgoing links).
    pub fn sinks(mut self) -> Self {
        let sinks = self.graph.internal_graph().find_sinks();
        self.ids.retain(|id| sinks.contains(id));
        self
    }

    /// Filter with a predicate on processor nodes.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ProcessorNode) -> bool,
    {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| predicate(n))
                .unwrap_or(false)
        });
        self
    }

    /// Filter to processors that have a specific component.
    pub fn has_component<C: Send + Sync + 'static>(mut self) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_processor(id)
                .map(|n| n.has::<C>())
                .unwrap_or(false)
        });
        self
    }

    // =========================================================================
    // Traversal Operations (weight -> weight)
    // =========================================================================

    /// Traverse to downstream processors.
    pub fn downstream(self) -> Self {
        let new_ids: Vec<_> = {
            let graph = self.graph.internal_graph();
            self.ids
                .iter()
                .flat_map(|id| {
                    graph
                        .links()
                        .iter()
                        .filter(|l| l.source.node.as_str() == id.as_str())
                        .map(|l| l.target.node.clone())
                })
                .collect()
        };
        ProcessorQueryMut {
            graph: self.graph,
            ids: new_ids,
        }
    }

    /// Traverse to upstream processors.
    pub fn upstream(self) -> Self {
        let new_ids: Vec<_> = {
            let graph = self.graph.internal_graph();
            self.ids
                .iter()
                .flat_map(|id| {
                    graph
                        .links()
                        .iter()
                        .filter(|l| l.target.node.as_str() == id.as_str())
                        .map(|l| l.source.node.clone())
                })
                .collect()
        };
        ProcessorQueryMut {
            graph: self.graph,
            ids: new_ids,
        }
    }

    /// Get outgoing links from current processors (mutable).
    pub fn out_links(self) -> LinkQueryMut<'a> {
        let link_ids: Vec<_> = {
            let graph = self.graph.internal_graph();
            self.ids
                .iter()
                .flat_map(|id| {
                    graph
                        .links()
                        .iter()
                        .filter(|l| l.source.node.as_str() == id.as_str())
                        .map(|l| l.id.clone())
                })
                .collect()
        };
        LinkQueryMut {
            graph: self.graph,
            ids: link_ids,
        }
    }

    /// Get incoming links to current processors (mutable).
    pub fn in_links(self) -> LinkQueryMut<'a> {
        let link_ids: Vec<_> = {
            let graph = self.graph.internal_graph();
            self.ids
                .iter()
                .flat_map(|id| {
                    graph
                        .links()
                        .iter()
                        .filter(|l| l.target.node.as_str() == id.as_str())
                        .map(|l| l.id.clone())
                })
                .collect()
        };
        LinkQueryMut {
            graph: self.graph,
            ids: link_ids,
        }
    }

    // =========================================================================
    // Terminal Operations (weight -> data, exits graph)
    // =========================================================================

    /// Return processor IDs.
    pub fn ids(self) -> Vec<ProcessorId> {
        self.ids
    }

    /// Return count of matching processors.
    pub fn count(self) -> usize {
        self.ids.len()
    }

    /// Return first matching processor ID.
    pub fn first(self) -> Option<ProcessorId> {
        self.ids.into_iter().next()
    }

    /// Check if any processors match.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }

    // =========================================================================
    // Mutation Terminals
    // =========================================================================

    /// Execute a mutation on each matching processor.
    pub fn for_each<F>(self, mut f: F)
    where
        F: FnMut(&mut ProcessorNode),
    {
        for id in self.ids {
            if let Some(node) = self.graph.internal_graph_mut().get_processor_mut(&id) {
                f(node);
            }
        }
    }

    /// Get mutable reference to the first matching processor.
    pub fn first_mut(self) -> Option<&'a mut ProcessorNode> {
        let id = self.ids.into_iter().next()?;
        self.graph.internal_graph_mut().get_processor_mut(&id)
    }
}

// =============================================================================
// Link Query (Read)
// =============================================================================

/// Read-only query over links.
pub struct LinkQuery<'a> {
    graph: &'a Graph,
    ids: Vec<LinkId>,
}

impl<'a> LinkQuery<'a> {
    // =========================================================================
    // Filter Operations
    // =========================================================================

    /// Filter with a predicate on links.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Link) -> bool,
    {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_link(id)
                .map(|l| predicate(l))
                .unwrap_or(false)
        });
        self
    }

    /// Filter to links that have a specific component.
    pub fn has_component<C: Send + Sync + 'static>(mut self) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_link(id)
                .map(|l| l.has::<C>())
                .unwrap_or(false)
        });
        self
    }

    // =========================================================================
    // Traversal Operations
    // =========================================================================

    /// Get source processors of current links.
    pub fn source_processors(self) -> ProcessorQuery<'a> {
        let proc_ids: Vec<_> = self
            .ids
            .iter()
            .filter_map(|id| {
                self.graph
                    .internal_graph()
                    .get_link(id)
                    .map(|l| ProcessorId::from(l.source.node.as_str()))
            })
            .collect();
        ProcessorQuery {
            graph: self.graph,
            ids: proc_ids,
        }
    }

    /// Get target processors of current links.
    pub fn target_processors(self) -> ProcessorQuery<'a> {
        let proc_ids: Vec<_> = self
            .ids
            .iter()
            .filter_map(|id| {
                self.graph
                    .internal_graph()
                    .get_link(id)
                    .map(|l| ProcessorId::from(l.target.node.as_str()))
            })
            .collect();
        ProcessorQuery {
            graph: self.graph,
            ids: proc_ids,
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Return link IDs.
    pub fn ids(self) -> Vec<LinkId> {
        self.ids
    }

    /// Return count of matching links.
    pub fn count(self) -> usize {
        self.ids.len()
    }

    /// Return first matching link ID.
    pub fn first(self) -> Option<LinkId> {
        self.ids.into_iter().next()
    }

    /// Check if any links match.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }

    /// Get reference to the first matching link.
    pub fn first(self) -> Option<&'a Link> {
        let id = self.ids.into_iter().next()?;
        self.graph.internal_graph().get_link(&id)
    }

    /// Iterate over matching links.
    pub fn iter(self) -> impl Iterator<Item = &'a Link> {
        let graph = self.graph;
        self.ids
            .into_iter()
            .filter_map(move |id| graph.internal_graph().get_link(&id))
    }
}

// =============================================================================
// Link Query (Mutable)
// =============================================================================

/// Mutable query over links.
pub struct LinkQueryMut<'a> {
    graph: &'a mut Graph,
    ids: Vec<LinkId>,
}

impl<'a> LinkQueryMut<'a> {
    // =========================================================================
    // Filter Operations
    // =========================================================================

    /// Filter with a predicate on links.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Link) -> bool,
    {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_link(id)
                .map(|l| predicate(l))
                .unwrap_or(false)
        });
        self
    }

    /// Filter to links that have a specific component.
    pub fn has_component<C: Send + Sync + 'static>(mut self) -> Self {
        self.ids.retain(|id| {
            self.graph
                .internal_graph()
                .get_link(id)
                .map(|l| l.has::<C>())
                .unwrap_or(false)
        });
        self
    }

    // =========================================================================
    // Traversal Operations
    // =========================================================================

    /// Get source processors of current links (mutable).
    pub fn source_processors(self) -> ProcessorQueryMut<'a> {
        let proc_ids: Vec<_> = self
            .ids
            .iter()
            .filter_map(|id| {
                self.graph
                    .internal_graph()
                    .get_link(id)
                    .map(|l| ProcessorId::from(l.source.node.as_str()))
            })
            .collect();
        ProcessorQueryMut {
            graph: self.graph,
            ids: proc_ids,
        }
    }

    /// Get target processors of current links (mutable).
    pub fn target_processors(self) -> ProcessorQueryMut<'a> {
        let proc_ids: Vec<_> = self
            .ids
            .iter()
            .filter_map(|id| {
                self.graph
                    .internal_graph()
                    .get_link(id)
                    .map(|l| ProcessorId::from(l.target.node.as_str()))
            })
            .collect();
        ProcessorQueryMut {
            graph: self.graph,
            ids: proc_ids,
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Return link IDs.
    pub fn ids(self) -> Vec<LinkId> {
        self.ids
    }

    /// Return count of matching links.
    pub fn count(self) -> usize {
        self.ids.len()
    }

    /// Return first matching link ID.
    pub fn first(self) -> Option<LinkId> {
        self.ids.into_iter().next()
    }

    /// Check if any links match.
    pub fn exists(self) -> bool {
        !self.ids.is_empty()
    }

    // =========================================================================
    // Mutation Terminals
    // =========================================================================

    /// Execute a mutation on each matching link.
    pub fn for_each<F>(self, mut f: F)
    where
        F: FnMut(&mut Link),
    {
        for id in self.ids {
            if let Some(link) = self.graph.internal_graph_mut().get_link_mut(&id) {
                f(link);
            }
        }
    }

    /// Get mutable reference to the first matching link.
    pub fn first_mut(self) -> Option<&'a mut Link> {
        let id = self.ids.into_iter().next()?;
        self.graph.internal_graph_mut().get_link_mut(&id)
    }
}
