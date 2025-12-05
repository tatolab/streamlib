// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph query interface.
//!
//! **STATUS: DESIGN ONLY - NOT YET IMPLEMENTED**
//!
//! This module contains trait definitions and query builder types for a future
//! graph query interface. The design captures how users would interact with
//! the graph without knowing about internal data structures.
//!
//! See `README.md` in this directory for the full design document.
//!
//! # Overview
//!
//! The query interface provides a Gremlin-inspired fluent API:
//!
//! ```ignore
//! // Future API - not yet implemented
//! let running_cameras = graph.query()
//!     .V()                                    // All processors
//!     .of_type("CameraProcessor")             // Filter by type
//!     .in_state(ProcessorState::Running)      // Filter by state
//!     .ids();                                 // Execute query
//! ```
//!
//! # Architecture
//!
//! - [`GraphQueryInterface`] - Trait defining primitive operations
//! - [`GraphQuery`] - Entry point returned by `graph.query()`
//! - [`ProcessorQuery`] - Lazy query builder for processors
//! - [`LinkQuery`] - Lazy query builder for links

mod traits;

pub use traits::{
    GraphQuery, GraphQueryInterface, LinkQuery, LinkSelection, ProcessorQuery, ProcessorSelection,
};
