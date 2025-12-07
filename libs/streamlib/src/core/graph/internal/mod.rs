// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Internal implementation details for the Graph.
//!
//! This module contains the backing stores for the public [`Graph`](super::Graph) API:
//! - [`InternalProcessorLinkGraph`] - petgraph-based topology (nodes and edges with embedded components)
//!
//! **Do not use these types directly** - use the public `Graph` API instead.

pub mod processor_link_graph;
mod processor_link_graph_query;

pub(crate) use processor_link_graph::{GraphChecksum, InternalProcessorLinkGraph};
