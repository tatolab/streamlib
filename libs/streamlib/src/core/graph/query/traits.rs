// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph query interface traits.
//!
//! **STATUS: DESIGN ONLY - NOT YET IMPLEMENTED**
//!
//! These traits define the contract for querying the graph without knowledge
//! of internal data structures (petgraph, hecs). See `README.md` for full design.

use std::collections::HashSet;

use crate::core::graph::{Link, ProcessorId, ProcessorNode};
use crate::core::links::LinkId;
use crate::core::processors::ProcessorState;

// =============================================================================
// GraphQueryInterface - The primitive operations trait
// =============================================================================

/// Primitive operations required to execute graph queries.
///
/// This trait is implemented by [`Graph`] and provides the low-level operations
/// that query builders use internally. Users don't call these directly - they
/// use the fluent query API instead.
///
/// # Design Notes
///
/// This trait abstracts over the internal storage (petgraph + hecs) so that:
/// 1. Query execution doesn't depend on storage implementation details
/// 2. We could swap storage backends without changing query code
/// 3. Testing can use mock implementations
///
/// Methods return owned types (`Vec`, `Option<T>`) rather than references
/// to avoid lifetime complexity in the query builder chain.
pub trait GraphQueryInterface {
    // =========================================================================
    // Processor (Vertex) Operations
    // =========================================================================

    /// Get all processor IDs in the graph.
    fn all_processor_ids(&self) -> Vec<ProcessorId>;

    /// Get a processor node by ID.
    fn get_processor_node(&self, id: &ProcessorId) -> Option<ProcessorNode>;

    /// Check if a processor exists.
    fn has_processor(&self, id: &ProcessorId) -> bool;

    /// Get the processor type (e.g., "CameraProcessor", "H264Encoder").
    fn get_processor_type(&self, id: &ProcessorId) -> Option<String>;

    /// Get the processor state from ECS.
    fn get_processor_state(&self, id: &ProcessorId) -> Option<ProcessorState>;

    /// Get processor config as JSON.
    fn get_processor_config(&self, id: &ProcessorId) -> Option<serde_json::Value>;

    // =========================================================================
    // Link (Edge) Operations
    // =========================================================================

    /// Get all link IDs in the graph.
    fn all_link_ids(&self) -> Vec<LinkId>;

    /// Get a link by ID.
    fn get_link(&self, id: &LinkId) -> Option<Link>;

    /// Check if a link exists.
    fn has_link(&self, id: &LinkId) -> bool;

    /// Get the source processor of a link.
    fn get_link_source(&self, id: &LinkId) -> Option<ProcessorId>;

    /// Get the target processor of a link.
    fn get_link_target(&self, id: &LinkId) -> Option<ProcessorId>;

    // =========================================================================
    // Traversal Operations
    // =========================================================================

    /// Get IDs of processors downstream of the given processor (outgoing links).
    fn downstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId>;

    /// Get IDs of processors upstream of the given processor (incoming links).
    fn upstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId>;

    /// Get IDs of outgoing links from a processor.
    fn outgoing_link_ids(&self, id: &ProcessorId) -> Vec<LinkId>;

    /// Get IDs of incoming links to a processor.
    fn incoming_link_ids(&self, id: &ProcessorId) -> Vec<LinkId>;

    /// Get processors in topological order (sources first, sinks last).
    fn topological_order(&self) -> Option<Vec<ProcessorId>>;

    /// Get source processors (no incoming links).
    fn source_processor_ids(&self) -> Vec<ProcessorId>;

    /// Get sink processors (no outgoing links).
    fn sink_processor_ids(&self) -> Vec<ProcessorId>;
}

// =============================================================================
// Query Builder Types (Sketch)
// =============================================================================

/// A lazy query over processors.
///
/// Built up through method chaining, executed when a terminal method is called.
///
/// # Example (Future API)
///
/// ```ignore
/// let encoder_ids = graph.query()
///     .V()                              // Start: all processors
///     .of_type("H264Encoder")           // Filter: by type
///     .in_state(ProcessorState::Running) // Filter: by state
///     .ids();                           // Terminal: execute and collect
/// ```
pub struct ProcessorQuery<'g, G: GraphQueryInterface> {
    /// Reference to the graph being queried.
    graph: &'g G,

    /// Current set of processor IDs in the query.
    /// None means "all processors" (lazy - not yet materialized).
    selection: ProcessorSelection,
}

/// Represents the current processor selection in a query.
#[derive(Clone)]
pub enum ProcessorSelection {
    /// All processors (not yet filtered).
    All,

    /// Specific set of processor IDs.
    Ids(HashSet<ProcessorId>),

    /// Empty selection (filters eliminated everything).
    Empty,
}

/// A lazy query over links.
///
/// Similar to [`ProcessorQuery`] but for link traversal.
pub struct LinkQuery<'g, G: GraphQueryInterface> {
    /// Reference to the graph being queried.
    graph: &'g G,

    /// Current set of link IDs in the query.
    selection: LinkSelection,
}

/// Represents the current link selection in a query.
#[derive(Clone)]
pub enum LinkSelection {
    /// All links.
    All,

    /// Specific set of link IDs.
    Ids(HashSet<LinkId>),

    /// Empty selection.
    Empty,
}

// =============================================================================
// ProcessorQuery Implementation Sketch
// =============================================================================

impl<'g, G: GraphQueryInterface> ProcessorQuery<'g, G> {
    /// Create a new query starting from all processors.
    pub fn all(graph: &'g G) -> Self {
        Self {
            graph,
            selection: ProcessorSelection::All,
        }
    }

    /// Create a new query starting from specific processors.
    pub fn from_ids(graph: &'g G, ids: impl IntoIterator<Item = ProcessorId>) -> Self {
        Self {
            graph,
            selection: ProcessorSelection::Ids(ids.into_iter().collect()),
        }
    }

    // =========================================================================
    // Filter Steps
    // =========================================================================

    /// Filter to processors of a specific type.
    pub fn of_type(self, processor_type: &str) -> Self {
        let ids = self.materialize_ids();
        let filtered: HashSet<_> = ids
            .into_iter()
            .filter(|id| {
                self.graph
                    .get_processor_type(id)
                    .map(|t| t == processor_type)
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            selection: if filtered.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(filtered)
            },
        }
    }

    /// Filter to processors in a specific state.
    pub fn in_state(self, state: ProcessorState) -> Self {
        let ids = self.materialize_ids();
        let filtered: HashSet<_> = ids
            .into_iter()
            .filter(|id| self.graph.get_processor_state(id) == Some(state))
            .collect();

        Self {
            graph: self.graph,
            selection: if filtered.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(filtered)
            },
        }
    }

    /// Filter to source processors (no incoming links).
    pub fn sources(self) -> Self {
        let source_ids: HashSet<_> = self.graph.source_processor_ids().into_iter().collect();
        let ids = self.materialize_ids();
        let filtered: HashSet<_> = ids.intersection(&source_ids).cloned().collect();

        Self {
            graph: self.graph,
            selection: if filtered.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(filtered)
            },
        }
    }

    /// Filter to sink processors (no outgoing links).
    pub fn sinks(self) -> Self {
        let sink_ids: HashSet<_> = self.graph.sink_processor_ids().into_iter().collect();
        let ids = self.materialize_ids();
        let filtered: HashSet<_> = ids.intersection(&sink_ids).cloned().collect();

        Self {
            graph: self.graph,
            selection: if filtered.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(filtered)
            },
        }
    }

    /// Filter with a custom predicate on processor nodes.
    pub fn filter<F>(self, predicate: F) -> Self
    where
        F: Fn(&ProcessorNode) -> bool,
    {
        let ids = self.materialize_ids();
        let filtered: HashSet<_> = ids
            .into_iter()
            .filter(|id| {
                self.graph
                    .get_processor_node(id)
                    .map(|node| predicate(&node))
                    .unwrap_or(false)
            })
            .collect();

        Self {
            graph: self.graph,
            selection: if filtered.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(filtered)
            },
        }
    }

    // =========================================================================
    // Traversal Steps
    // =========================================================================

    /// Traverse to downstream processors (follow outgoing links).
    pub fn downstream(self) -> Self {
        let ids = self.materialize_ids();
        let downstream: HashSet<_> = ids
            .into_iter()
            .flat_map(|id| self.graph.downstream_processor_ids(&id))
            .collect();

        Self {
            graph: self.graph,
            selection: if downstream.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(downstream)
            },
        }
    }

    /// Traverse to upstream processors (follow incoming links).
    pub fn upstream(self) -> Self {
        let ids = self.materialize_ids();
        let upstream: HashSet<_> = ids
            .into_iter()
            .flat_map(|id| self.graph.upstream_processor_ids(&id))
            .collect();

        Self {
            graph: self.graph,
            selection: if upstream.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(upstream)
            },
        }
    }

    /// Get outgoing links from the current processors.
    pub fn out_links(self) -> LinkQuery<'g, G> {
        let ids = self.materialize_ids();
        let link_ids: HashSet<_> = ids
            .into_iter()
            .flat_map(|id| self.graph.outgoing_link_ids(&id))
            .collect();

        LinkQuery {
            graph: self.graph,
            selection: if link_ids.is_empty() {
                LinkSelection::Empty
            } else {
                LinkSelection::Ids(link_ids)
            },
        }
    }

    /// Get incoming links to the current processors.
    pub fn in_links(self) -> LinkQuery<'g, G> {
        let ids = self.materialize_ids();
        let link_ids: HashSet<_> = ids
            .into_iter()
            .flat_map(|id| self.graph.incoming_link_ids(&id))
            .collect();

        LinkQuery {
            graph: self.graph,
            selection: if link_ids.is_empty() {
                LinkSelection::Empty
            } else {
                LinkSelection::Ids(link_ids)
            },
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Execute the query and return processor IDs.
    pub fn ids(self) -> Vec<ProcessorId> {
        match self.selection {
            ProcessorSelection::All => self.graph.all_processor_ids(),
            ProcessorSelection::Ids(ids) => ids.into_iter().collect(),
            ProcessorSelection::Empty => Vec::new(),
        }
    }

    /// Execute the query and return the count.
    pub fn count(self) -> usize {
        match self.selection {
            ProcessorSelection::All => self.graph.all_processor_ids().len(),
            ProcessorSelection::Ids(ids) => ids.len(),
            ProcessorSelection::Empty => 0,
        }
    }

    /// Execute the query and return the first result.
    pub fn first(self) -> Option<ProcessorId> {
        match self.selection {
            ProcessorSelection::All => self.graph.all_processor_ids().into_iter().next(),
            ProcessorSelection::Ids(ids) => ids.into_iter().next(),
            ProcessorSelection::Empty => None,
        }
    }

    /// Check if the query matches any processors.
    pub fn exists(self) -> bool {
        match self.selection {
            ProcessorSelection::All => !self.graph.all_processor_ids().is_empty(),
            ProcessorSelection::Ids(ids) => !ids.is_empty(),
            ProcessorSelection::Empty => false,
        }
    }

    /// Execute and return full processor nodes.
    pub fn nodes(self) -> Vec<ProcessorNode> {
        let graph = self.graph;
        self.ids()
            .into_iter()
            .filter_map(|id| graph.get_processor_node(&id))
            .collect()
    }

    // =========================================================================
    // Internal Helpers
    // =========================================================================

    /// Materialize the current selection to a concrete set of IDs.
    fn materialize_ids(&self) -> HashSet<ProcessorId> {
        match &self.selection {
            ProcessorSelection::All => self.graph.all_processor_ids().into_iter().collect(),
            ProcessorSelection::Ids(ids) => ids.clone(),
            ProcessorSelection::Empty => HashSet::new(),
        }
    }
}

// =============================================================================
// LinkQuery Implementation Sketch
// =============================================================================

impl<'g, G: GraphQueryInterface> LinkQuery<'g, G> {
    /// Create a new query starting from all links.
    pub fn all(graph: &'g G) -> Self {
        Self {
            graph,
            selection: LinkSelection::All,
        }
    }

    // =========================================================================
    // Traversal Steps
    // =========================================================================

    /// Get the source processors of the current links.
    pub fn source_processors(self) -> ProcessorQuery<'g, G> {
        let link_ids = self.materialize_ids();
        let processor_ids: HashSet<_> = link_ids
            .into_iter()
            .filter_map(|id| self.graph.get_link_source(&id))
            .collect();

        ProcessorQuery {
            graph: self.graph,
            selection: if processor_ids.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(processor_ids)
            },
        }
    }

    /// Get the target processors of the current links.
    pub fn target_processors(self) -> ProcessorQuery<'g, G> {
        let link_ids = self.materialize_ids();
        let processor_ids: HashSet<_> = link_ids
            .into_iter()
            .filter_map(|id| self.graph.get_link_target(&id))
            .collect();

        ProcessorQuery {
            graph: self.graph,
            selection: if processor_ids.is_empty() {
                ProcessorSelection::Empty
            } else {
                ProcessorSelection::Ids(processor_ids)
            },
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Execute the query and return link IDs.
    pub fn ids(self) -> Vec<LinkId> {
        match self.selection {
            LinkSelection::All => self.graph.all_link_ids(),
            LinkSelection::Ids(ids) => ids.into_iter().collect(),
            LinkSelection::Empty => Vec::new(),
        }
    }

    /// Execute the query and return the count.
    pub fn count(self) -> usize {
        match self.selection {
            LinkSelection::All => self.graph.all_link_ids().len(),
            LinkSelection::Ids(ids) => ids.len(),
            LinkSelection::Empty => 0,
        }
    }

    /// Execute the query and return the first result.
    pub fn first(self) -> Option<LinkId> {
        match self.selection {
            LinkSelection::All => self.graph.all_link_ids().into_iter().next(),
            LinkSelection::Ids(ids) => ids.into_iter().next(),
            LinkSelection::Empty => None,
        }
    }

    /// Execute and return full link objects.
    pub fn links(self) -> Vec<Link> {
        let graph = self.graph;
        self.ids()
            .into_iter()
            .filter_map(|id| graph.get_link(&id))
            .collect()
    }

    // =========================================================================
    // Internal Helpers
    // =========================================================================

    fn materialize_ids(&self) -> HashSet<LinkId> {
        match &self.selection {
            LinkSelection::All => self.graph.all_link_ids().into_iter().collect(),
            LinkSelection::Ids(ids) => ids.clone(),
            LinkSelection::Empty => HashSet::new(),
        }
    }
}

// =============================================================================
// Query Entry Point (would be added to Graph)
// =============================================================================

/// Entry point for building graph queries.
///
/// Returned by `Graph::query()` to start a fluent query chain.
pub struct GraphQuery<'g, G: GraphQueryInterface> {
    graph: &'g G,
}

impl<'g, G: GraphQueryInterface> GraphQuery<'g, G> {
    /// Create a new query entry point.
    pub fn new(graph: &'g G) -> Self {
        Self { graph }
    }

    /// Start a query from all processors (vertices).
    ///
    /// Gremlin equivalent: `g.V()`
    #[allow(non_snake_case)]
    pub fn V(self) -> ProcessorQuery<'g, G> {
        ProcessorQuery::all(self.graph)
    }

    /// Start a query from specific processors by ID.
    ///
    /// Gremlin equivalent: `g.V(id1, id2, ...)`
    #[allow(non_snake_case)]
    pub fn V_from(self, ids: impl IntoIterator<Item = ProcessorId>) -> ProcessorQuery<'g, G> {
        ProcessorQuery::from_ids(self.graph, ids)
    }

    /// Start a query from all links (edges).
    ///
    /// Gremlin equivalent: `g.E()`
    #[allow(non_snake_case)]
    pub fn E(self) -> LinkQuery<'g, G> {
        LinkQuery::all(self.graph)
    }
}

// =============================================================================
// Future: Component-aware queries
// =============================================================================

// These would require generic component access in GraphQueryInterface.
// Sketched here for future reference.

/*
pub trait GraphQueryInterfaceExt: GraphQueryInterface {
    /// Check if a processor has a specific component type.
    fn processor_has_component<C: Component>(&self, id: &ProcessorId) -> bool;

    /// Get a component value (cloned) from a processor.
    fn get_processor_component<C: Component + Clone>(&self, id: &ProcessorId) -> Option<C>;
}

impl<'g, G: GraphQueryInterfaceExt> ProcessorQuery<'g, G> {
    /// Filter to processors that have a specific component.
    pub fn with_component<C: Component>(self) -> Self { ... }

    /// Filter by component value.
    pub fn where_component<C, F>(self, predicate: F) -> Self
    where
        C: Component + Clone,
        F: Fn(&C) -> bool,
    { ... }
}
*/
