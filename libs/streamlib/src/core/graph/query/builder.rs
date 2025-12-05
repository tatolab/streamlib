// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Query builder types for the detached query pattern.
//!
//! Queries are built without a graph reference using `Query::build()`,
//! then executed against a graph with `graph.execute(&query)`.

use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::core::graph::{Link, ProcessorId, ProcessorNode};
use crate::core::links::LinkId;
use crate::core::processors::ProcessorState;

// =============================================================================
// Query Entry Point
// =============================================================================

/// Entry point for building graph queries.
///
/// # Example
///
/// ```ignore
/// let query = Query::build()
///     .V()
///     .of_type("CameraProcessor")
///     .downstream()
///     .ids();
///
/// let result = graph.execute(&query);
/// ```
pub struct Query;

impl Query {
    /// Start building a new query.
    pub fn build() -> QueryBuilder {
        QueryBuilder
    }
}

/// Initial query builder that selects vertices or edges.
pub struct QueryBuilder;

impl QueryBuilder {
    /// Start from all processors (vertices).
    ///
    /// Gremlin equivalent: `g.V()`
    #[allow(non_snake_case)]
    pub fn V(self) -> ProcessorQueryBuilder {
        ProcessorQueryBuilder {
            start: ProcessorStart::All,
            steps: Vec::new(),
        }
    }

    /// Start from specific processors by ID.
    ///
    /// Gremlin equivalent: `g.V(id1, id2, ...)`
    #[allow(non_snake_case)]
    pub fn V_from(self, ids: impl IntoIterator<Item = ProcessorId>) -> ProcessorQueryBuilder {
        ProcessorQueryBuilder {
            start: ProcessorStart::Ids(ids.into_iter().collect()),
            steps: Vec::new(),
        }
    }

    /// Start from all links (edges).
    ///
    /// Gremlin equivalent: `g.E()`
    #[allow(non_snake_case)]
    pub fn E(self) -> LinkQueryBuilder {
        LinkQueryBuilder {
            start: LinkStart::All,
            steps: Vec::new(),
        }
    }
}

// =============================================================================
// Processor Query Builder
// =============================================================================

/// Starting point for a processor query.
pub enum ProcessorStart {
    /// All processors.
    All,
    /// Specific processor IDs.
    Ids(Vec<ProcessorId>),
    /// Source processors of links from a link query.
    FromLinkSources(Box<LinkQueryBuilder>),
    /// Target processors of links from a link query.
    FromLinkTargets(Box<LinkQueryBuilder>),
}

/// A step in a processor query pipeline.
pub enum ProcessorStep {
    /// Filter by processor type.
    OfType(String),
    /// Filter by processor state.
    InState(ProcessorState),
    /// Filter to source processors (no incoming links).
    Sources,
    /// Filter to sink processors (no outgoing links).
    Sinks,
    /// Filter with custom predicate on ProcessorNode.
    Filter(Arc<dyn Fn(&ProcessorNode) -> bool + Send + Sync>),
    /// Filter by field value.
    WhereField {
        path: String,
        predicate: Arc<dyn Fn(&JsonValue) -> bool + Send + Sync>,
    },
    /// Filter to processors that have a specific field.
    HasField(String),
    /// Traverse to downstream processors.
    Downstream,
    /// Traverse to upstream processors.
    Upstream,
}

/// Builder for processor queries.
///
/// Accumulates steps that will be executed when the query runs.
pub struct ProcessorQueryBuilder {
    pub(crate) start: ProcessorStart,
    pub(crate) steps: Vec<ProcessorStep>,
}

impl ProcessorQueryBuilder {
    // =========================================================================
    // Filter Steps
    // =========================================================================

    /// Filter to processors of a specific type.
    pub fn of_type(mut self, processor_type: impl Into<String>) -> Self {
        self.steps
            .push(ProcessorStep::OfType(processor_type.into()));
        self
    }

    /// Filter to processors in a specific state.
    pub fn in_state(mut self, state: ProcessorState) -> Self {
        self.steps.push(ProcessorStep::InState(state));
        self
    }

    /// Filter to source processors (no incoming links).
    pub fn sources(mut self) -> Self {
        self.steps.push(ProcessorStep::Sources);
        self
    }

    /// Filter to sink processors (no outgoing links).
    pub fn sinks(mut self) -> Self {
        self.steps.push(ProcessorStep::Sinks);
        self
    }

    /// Filter with a custom predicate on processor nodes.
    pub fn filter<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ProcessorNode) -> bool + Send + Sync + 'static,
    {
        self.steps.push(ProcessorStep::Filter(Arc::new(predicate)));
        self
    }

    /// Filter by a field value using a predicate.
    ///
    /// Field paths use dot notation matching the JSON output structure:
    /// - `"type"`, `"state"`, `"metrics.throughput_fps"`, `"config.bitrate"`
    pub fn where_field<F>(mut self, path: impl Into<String>, predicate: F) -> Self
    where
        F: Fn(&JsonValue) -> bool + Send + Sync + 'static,
    {
        self.steps.push(ProcessorStep::WhereField {
            path: path.into(),
            predicate: Arc::new(predicate),
        });
        self
    }

    /// Filter to processors that have a specific field.
    pub fn has_field(mut self, path: impl Into<String>) -> Self {
        self.steps.push(ProcessorStep::HasField(path.into()));
        self
    }

    // =========================================================================
    // Traversal Steps
    // =========================================================================

    /// Traverse to downstream processors (follow outgoing links).
    pub fn downstream(mut self) -> Self {
        self.steps.push(ProcessorStep::Downstream);
        self
    }

    /// Traverse to upstream processors (follow incoming links).
    pub fn upstream(mut self) -> Self {
        self.steps.push(ProcessorStep::Upstream);
        self
    }

    /// Get outgoing links from the current processors.
    pub fn out_links(self) -> LinkQueryBuilder {
        LinkQueryBuilder {
            start: LinkStart::FromProcessors {
                query: Box::new(self),
                direction: LinkDirection::Outgoing,
            },
            steps: Vec::new(),
        }
    }

    /// Get incoming links to the current processors.
    pub fn in_links(self) -> LinkQueryBuilder {
        LinkQueryBuilder {
            start: LinkStart::FromProcessors {
                query: Box::new(self),
                direction: LinkDirection::Incoming,
            },
            steps: Vec::new(),
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Finalize query to return processor IDs.
    pub fn ids(self) -> ProcessorQuery<Vec<ProcessorId>> {
        ProcessorQuery {
            start: self.start,
            steps: self.steps,
            terminal: ProcessorTerminal::Ids,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return count.
    pub fn count(self) -> ProcessorQuery<usize> {
        ProcessorQuery {
            start: self.start,
            steps: self.steps,
            terminal: ProcessorTerminal::Count,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return first result.
    pub fn first(self) -> ProcessorQuery<Option<ProcessorId>> {
        ProcessorQuery {
            start: self.start,
            steps: self.steps,
            terminal: ProcessorTerminal::First,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to check if any processors match.
    pub fn exists(self) -> ProcessorQuery<bool> {
        ProcessorQuery {
            start: self.start,
            steps: self.steps,
            terminal: ProcessorTerminal::Exists,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return full processor nodes.
    pub fn nodes(self) -> ProcessorQuery<Vec<ProcessorNode>> {
        ProcessorQuery {
            start: self.start,
            steps: self.steps,
            terminal: ProcessorTerminal::Nodes,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Terminal operation for processor queries.
#[derive(Clone, Copy)]
pub enum ProcessorTerminal {
    Ids,
    Count,
    First,
    Exists,
    Nodes,
}

/// A finalized processor query ready for execution.
pub struct ProcessorQuery<T> {
    pub(crate) start: ProcessorStart,
    pub(crate) steps: Vec<ProcessorStep>,
    pub(crate) terminal: ProcessorTerminal,
    pub(crate) _phantom: std::marker::PhantomData<T>,
}

// =============================================================================
// Link Query Builder
// =============================================================================

/// Direction for link traversal from processors.
#[derive(Clone, Copy)]
pub enum LinkDirection {
    Outgoing,
    Incoming,
}

/// Starting point for a link query.
pub enum LinkStart {
    /// All links.
    All,
    /// Links from a processor query.
    FromProcessors {
        query: Box<ProcessorQueryBuilder>,
        direction: LinkDirection,
    },
}

/// A step in a link query pipeline.
pub enum LinkStep {
    /// Filter by field value.
    WhereField {
        path: String,
        predicate: Arc<dyn Fn(&JsonValue) -> bool + Send + Sync>,
    },
    /// Filter to links that have a specific field.
    HasField(String),
}

/// Builder for link queries.
pub struct LinkQueryBuilder {
    pub(crate) start: LinkStart,
    pub(crate) steps: Vec<LinkStep>,
}

impl LinkQueryBuilder {
    // =========================================================================
    // Filter Steps
    // =========================================================================

    /// Filter by a field value using a predicate.
    ///
    /// Field paths use dot notation matching the JSON output structure:
    /// - `"from.processor"`, `"type_info.capacity"`, `"buffer.fill_level"`
    pub fn where_field<F>(mut self, path: impl Into<String>, predicate: F) -> Self
    where
        F: Fn(&JsonValue) -> bool + Send + Sync + 'static,
    {
        self.steps.push(LinkStep::WhereField {
            path: path.into(),
            predicate: Arc::new(predicate),
        });
        self
    }

    /// Filter to links that have a specific field.
    pub fn has_field(mut self, path: impl Into<String>) -> Self {
        self.steps.push(LinkStep::HasField(path.into()));
        self
    }

    // =========================================================================
    // Traversal Steps
    // =========================================================================

    /// Get the source processors of the current links.
    pub fn source_processors(self) -> ProcessorQueryBuilder {
        ProcessorQueryBuilder {
            start: ProcessorStart::FromLinkSources(Box::new(self)),
            steps: Vec::new(),
        }
    }

    /// Get the target processors of the current links.
    pub fn target_processors(self) -> ProcessorQueryBuilder {
        ProcessorQueryBuilder {
            start: ProcessorStart::FromLinkTargets(Box::new(self)),
            steps: Vec::new(),
        }
    }

    // =========================================================================
    // Terminal Operations
    // =========================================================================

    /// Finalize query to return link IDs.
    pub fn ids(self) -> LinkQuery<Vec<LinkId>> {
        LinkQuery {
            start: self.start,
            steps: self.steps,
            terminal: LinkTerminal::Ids,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return count.
    pub fn count(self) -> LinkQuery<usize> {
        LinkQuery {
            start: self.start,
            steps: self.steps,
            terminal: LinkTerminal::Count,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return first result.
    pub fn first(self) -> LinkQuery<Option<LinkId>> {
        LinkQuery {
            start: self.start,
            steps: self.steps,
            terminal: LinkTerminal::First,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to return full link objects.
    pub fn links(self) -> LinkQuery<Vec<Link>> {
        LinkQuery {
            start: self.start,
            steps: self.steps,
            terminal: LinkTerminal::Links,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Finalize query to check if any links exist.
    pub fn exists(self) -> LinkQuery<bool> {
        LinkQuery {
            start: self.start,
            steps: self.steps,
            terminal: LinkTerminal::Exists,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Terminal operation for link queries.
#[derive(Clone, Copy)]
pub enum LinkTerminal {
    Ids,
    Count,
    First,
    Links,
    Exists,
}

/// A finalized link query ready for execution.
pub struct LinkQuery<T> {
    pub(crate) start: LinkStart,
    pub(crate) steps: Vec<LinkStep>,
    pub(crate) terminal: LinkTerminal,
    pub(crate) _phantom: std::marker::PhantomData<T>,
}
