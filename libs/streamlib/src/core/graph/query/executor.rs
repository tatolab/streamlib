// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query execution traits and implementations.
//!
//! Defines the executor interface that graphs implement to run queries.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::graph::{Link, ProcessorId, ProcessorNode};
use crate::core::links::LinkId;
use crate::core::processors::ProcessorState;

use super::builder::{
    LinkDirection, LinkQuery, LinkQueryBuilder, LinkStart, LinkStep, LinkTerminal, ProcessorQuery,
    ProcessorQueryBuilder, ProcessorStart, ProcessorStep, ProcessorTerminal,
};
use super::field_resolver::{resolve_json_path, FieldResolver};

// =============================================================================
// GraphQueryInterface - Primitive operations for query execution
// =============================================================================

/// Primitive operations required to execute graph queries.
///
/// This trait abstracts over the internal storage (petgraph + hecs) so that:
/// 1. Query execution doesn't depend on storage implementation details
/// 2. Storage backends could be swapped without changing query code
/// 3. Testing can use mock implementations
pub trait GraphQueryInterface: FieldResolver {
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
    fn get_processor_config(&self, id: &ProcessorId) -> Option<JsonValue>;

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
// GraphQueryExecutor - Execute queries against a graph
// =============================================================================

/// Execute queries against a graph.
///
/// Implemented by [`Graph`] to run detached queries built with [`Query::build()`].
pub trait GraphQueryExecutor: GraphQueryInterface {
    /// Execute a processor query and return the result.
    fn execute_processor_query<T>(&self, query: &ProcessorQuery<T>) -> T
    where
        T: ProcessorQueryResult;

    /// Execute a link query and return the result.
    fn execute_link_query<T>(&self, query: &LinkQuery<T>) -> T
    where
        T: LinkQueryResult;
}

/// Marker trait for processor query result types.
pub trait ProcessorQueryResult: Sized {
    fn from_ids(ids: Vec<ProcessorId>, graph: &impl GraphQueryInterface) -> Self;
    fn from_terminal(
        terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        graph: &impl GraphQueryInterface,
    ) -> Self;
}

/// Marker trait for link query result types.
pub trait LinkQueryResult: Sized {
    fn from_ids(ids: Vec<LinkId>, graph: &impl GraphQueryInterface) -> Self;
    fn from_terminal(
        terminal: LinkTerminal,
        ids: Vec<LinkId>,
        graph: &impl GraphQueryInterface,
    ) -> Self;
}

// =============================================================================
// ProcessorQueryResult implementations
// =============================================================================

impl ProcessorQueryResult for Vec<ProcessorId> {
    fn from_ids(ids: Vec<ProcessorId>, _graph: &impl GraphQueryInterface) -> Self {
        ids
    }

    fn from_terminal(
        _terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids
    }
}

impl ProcessorQueryResult for usize {
    fn from_ids(ids: Vec<ProcessorId>, _graph: &impl GraphQueryInterface) -> Self {
        ids.len()
    }

    fn from_terminal(
        _terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.len()
    }
}

impl ProcessorQueryResult for Option<ProcessorId> {
    fn from_ids(ids: Vec<ProcessorId>, _graph: &impl GraphQueryInterface) -> Self {
        ids.into_iter().next()
    }

    fn from_terminal(
        _terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.into_iter().next()
    }
}

impl ProcessorQueryResult for bool {
    fn from_ids(ids: Vec<ProcessorId>, _graph: &impl GraphQueryInterface) -> Self {
        !ids.is_empty()
    }

    fn from_terminal(
        _terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        !ids.is_empty()
    }
}

impl ProcessorQueryResult for Vec<ProcessorNode> {
    fn from_ids(ids: Vec<ProcessorId>, graph: &impl GraphQueryInterface) -> Self {
        ids.into_iter()
            .filter_map(|id| graph.get_processor_node(&id))
            .collect()
    }

    fn from_terminal(
        _terminal: ProcessorTerminal,
        ids: Vec<ProcessorId>,
        graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.into_iter()
            .filter_map(|id| graph.get_processor_node(&id))
            .collect()
    }
}

// =============================================================================
// LinkQueryResult implementations
// =============================================================================

impl LinkQueryResult for Vec<LinkId> {
    fn from_ids(ids: Vec<LinkId>, _graph: &impl GraphQueryInterface) -> Self {
        ids
    }

    fn from_terminal(
        _terminal: LinkTerminal,
        ids: Vec<LinkId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids
    }
}

impl LinkQueryResult for usize {
    fn from_ids(ids: Vec<LinkId>, _graph: &impl GraphQueryInterface) -> Self {
        ids.len()
    }

    fn from_terminal(
        _terminal: LinkTerminal,
        ids: Vec<LinkId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.len()
    }
}

impl LinkQueryResult for Option<LinkId> {
    fn from_ids(ids: Vec<LinkId>, _graph: &impl GraphQueryInterface) -> Self {
        ids.into_iter().next()
    }

    fn from_terminal(
        _terminal: LinkTerminal,
        ids: Vec<LinkId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.into_iter().next()
    }
}

impl LinkQueryResult for Vec<Link> {
    fn from_ids(ids: Vec<LinkId>, graph: &impl GraphQueryInterface) -> Self {
        ids.into_iter()
            .filter_map(|id| graph.get_link(&id))
            .collect()
    }

    fn from_terminal(
        _terminal: LinkTerminal,
        ids: Vec<LinkId>,
        graph: &impl GraphQueryInterface,
    ) -> Self {
        ids.into_iter()
            .filter_map(|id| graph.get_link(&id))
            .collect()
    }
}

impl LinkQueryResult for bool {
    fn from_ids(ids: Vec<LinkId>, _graph: &impl GraphQueryInterface) -> Self {
        !ids.is_empty()
    }

    fn from_terminal(
        _terminal: LinkTerminal,
        ids: Vec<LinkId>,
        _graph: &impl GraphQueryInterface,
    ) -> Self {
        !ids.is_empty()
    }
}

// =============================================================================
// Query Execution Logic
// =============================================================================

/// Execute a processor query builder and return the resulting IDs.
pub fn execute_processor_query_builder(
    builder: &ProcessorQueryBuilder,
    graph: &impl GraphQueryInterface,
) -> Vec<ProcessorId> {
    // Get starting set of IDs
    let mut ids: HashSet<ProcessorId> = match &builder.start {
        ProcessorStart::All => graph.all_processor_ids().into_iter().collect(),
        ProcessorStart::Ids(ids) => ids.iter().cloned().collect(),
        ProcessorStart::FromLinkSources(link_builder) => {
            let link_ids = execute_link_query_builder(link_builder, graph);
            link_ids
                .into_iter()
                .filter_map(|id| graph.get_link_source(&id))
                .collect()
        }
        ProcessorStart::FromLinkTargets(link_builder) => {
            let link_ids = execute_link_query_builder(link_builder, graph);
            link_ids
                .into_iter()
                .filter_map(|id| graph.get_link_target(&id))
                .collect()
        }
    };

    // Apply each step
    for step in &builder.steps {
        ids = apply_processor_step(step, ids, graph);
        if ids.is_empty() {
            break;
        }
    }

    ids.into_iter().collect()
}

/// Execute a link query builder and return the resulting IDs.
pub fn execute_link_query_builder(
    builder: &LinkQueryBuilder,
    graph: &impl GraphQueryInterface,
) -> Vec<LinkId> {
    // Get starting set of IDs
    let mut ids: HashSet<LinkId> = match &builder.start {
        LinkStart::All => graph.all_link_ids().into_iter().collect(),
        LinkStart::FromProcessors { query, direction } => {
            let processor_ids = execute_processor_query_builder(query, graph);
            processor_ids
                .into_iter()
                .flat_map(|pid| match direction {
                    LinkDirection::Outgoing => graph.outgoing_link_ids(&pid),
                    LinkDirection::Incoming => graph.incoming_link_ids(&pid),
                })
                .collect()
        }
    };

    // Apply each step
    for step in &builder.steps {
        ids = apply_link_step(step, ids, graph);
        if ids.is_empty() {
            break;
        }
    }

    ids.into_iter().collect()
}

/// Execute a finalized processor query and return the resulting IDs.
pub fn execute_processor_query_full<T>(
    query: &ProcessorQuery<T>,
    graph: &impl GraphQueryInterface,
) -> Vec<ProcessorId> {
    // Get starting set of IDs
    let mut ids: HashSet<ProcessorId> = match &query.start {
        ProcessorStart::All => graph.all_processor_ids().into_iter().collect(),
        ProcessorStart::Ids(ids) => ids.iter().cloned().collect(),
        ProcessorStart::FromLinkSources(link_builder) => {
            let link_ids = execute_link_query_builder(link_builder, graph);
            link_ids
                .into_iter()
                .filter_map(|id| graph.get_link_source(&id))
                .collect()
        }
        ProcessorStart::FromLinkTargets(link_builder) => {
            let link_ids = execute_link_query_builder(link_builder, graph);
            link_ids
                .into_iter()
                .filter_map(|id| graph.get_link_target(&id))
                .collect()
        }
    };

    // Apply each step
    for step in &query.steps {
        ids = apply_processor_step(step, ids, graph);
        if ids.is_empty() {
            break;
        }
    }

    ids.into_iter().collect()
}

/// Execute a finalized link query and return the resulting IDs.
pub fn execute_link_query_full<T>(
    query: &LinkQuery<T>,
    graph: &impl GraphQueryInterface,
) -> Vec<LinkId> {
    // Get starting set of IDs
    let mut ids: HashSet<LinkId> = match &query.start {
        LinkStart::All => graph.all_link_ids().into_iter().collect(),
        LinkStart::FromProcessors { query, direction } => {
            let processor_ids = execute_processor_query_builder(query, graph);
            processor_ids
                .into_iter()
                .flat_map(|pid| match direction {
                    LinkDirection::Outgoing => graph.outgoing_link_ids(&pid),
                    LinkDirection::Incoming => graph.incoming_link_ids(&pid),
                })
                .collect()
        }
    };

    // Apply each step
    for step in &query.steps {
        ids = apply_link_step(step, ids, graph);
        if ids.is_empty() {
            break;
        }
    }

    ids.into_iter().collect()
}

/// Apply a single processor step to filter/traverse the ID set.
fn apply_processor_step(
    step: &ProcessorStep,
    ids: HashSet<ProcessorId>,
    graph: &impl GraphQueryInterface,
) -> HashSet<ProcessorId> {
    match step {
        ProcessorStep::OfType(processor_type) => ids
            .into_iter()
            .filter(|id| {
                graph
                    .get_processor_type(id)
                    .map(|t| &t == processor_type)
                    .unwrap_or(false)
            })
            .collect(),

        ProcessorStep::InState(state) => ids
            .into_iter()
            .filter(|id| graph.get_processor_state(id) == Some(*state))
            .collect(),

        ProcessorStep::Sources => {
            let sources: HashSet<_> = graph.source_processor_ids().into_iter().collect();
            ids.intersection(&sources).cloned().collect()
        }

        ProcessorStep::Sinks => {
            let sinks: HashSet<_> = graph.sink_processor_ids().into_iter().collect();
            ids.intersection(&sinks).cloned().collect()
        }

        ProcessorStep::Filter(predicate) => ids
            .into_iter()
            .filter(|id| {
                graph
                    .get_processor_node(id)
                    .map(|node| predicate(&node))
                    .unwrap_or(false)
            })
            .collect(),

        ProcessorStep::WhereField { path, predicate } => ids
            .into_iter()
            .filter(|id| {
                graph
                    .processor_to_json(id)
                    .and_then(|json| resolve_json_path(&json, path))
                    .map(|value| predicate(&value))
                    .unwrap_or(false)
            })
            .collect(),

        ProcessorStep::HasField(path) => ids
            .into_iter()
            .filter(|id| {
                graph
                    .processor_to_json(id)
                    .and_then(|json| resolve_json_path(&json, path))
                    .is_some()
            })
            .collect(),

        ProcessorStep::Downstream => ids
            .into_iter()
            .flat_map(|id| graph.downstream_processor_ids(&id))
            .collect(),

        ProcessorStep::Upstream => ids
            .into_iter()
            .flat_map(|id| graph.upstream_processor_ids(&id))
            .collect(),
    }
}

/// Apply a single link step to filter the ID set.
fn apply_link_step(
    step: &LinkStep,
    ids: HashSet<LinkId>,
    graph: &impl GraphQueryInterface,
) -> HashSet<LinkId> {
    match step {
        LinkStep::WhereField { path, predicate } => ids
            .into_iter()
            .filter(|id| {
                graph
                    .link_to_json(id)
                    .and_then(|json| resolve_json_path(&json, path))
                    .map(|value| predicate(&value))
                    .unwrap_or(false)
            })
            .collect(),

        LinkStep::HasField(path) => ids
            .into_iter()
            .filter(|id| {
                graph
                    .link_to_json(id)
                    .and_then(|json| resolve_json_path(&json, path))
                    .is_some()
            })
            .collect(),
    }
}
