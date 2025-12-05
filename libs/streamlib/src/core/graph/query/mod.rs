// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph query interface.
//!
//! Provides a unified query interface over property graph and ECS components.
//! Users see one graph with queryable properties on nodes/edges.
//!
//! # Overview
//!
//! Build reusable queries with `Query::build()`, then execute against a graph:
//!
//! ```ignore
//! use streamlib::core::graph::query::Query;
//!
//! // Build a reusable query
//! let camera_query = Query::build()
//!     .V()
//!     .of_type("CameraProcessor")
//!     .in_state(ProcessorState::Running)
//!     .ids();
//!
//! // Execute on a graph
//! let cameras = graph.execute(&camera_query);
//! ```
//!
//! # Architecture
//!
//! - [`Query`] - Entry point for building queries
//! - [`ProcessorQueryBuilder`] - Builder for processor queries
//! - [`LinkQueryBuilder`] - Builder for link queries
//! - [`ProcessorQuery`] / [`LinkQuery`] - Finalized queries ready for execution
//! - [`GraphQueryInterface`] - Primitive operations trait (implemented by Graph)
//! - [`FieldResolver`] - Unified field access across property graph and ECS

pub mod builder;
pub mod executor;
pub mod field_resolver;
mod traits;

#[cfg(test)]
mod tests;

// Re-export main types for convenience
pub use builder::{
    LinkQuery, LinkQueryBuilder, ProcessorQuery, ProcessorQueryBuilder, Query, QueryBuilder,
};
pub use executor::{
    GraphQueryExecutor, GraphQueryInterface, LinkQueryResult, ProcessorQueryResult,
};
pub use field_resolver::FieldResolver;
