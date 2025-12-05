// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph query interface traits.
//!
//! This module provides trait-based abstractions for querying the graph.
//!
//! The query interface is organized into:
//! - [`FieldResolver`] - Unified field access over property graph + ECS
//! - [`GraphQueryInterface`] - Primitive operations for query execution
//! - [`GraphQueryExecutor`] - Query execution on graph implementations
//!
//! See [`super::builder`] for the fluent query builder API.

// Traits are defined in their own modules and re-exported from query/mod.rs
// This file exists for documentation purposes.
