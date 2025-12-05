// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
#[allow(clippy::module_inception)]
mod graph;
#[doc(hidden)]
pub mod internal;
mod link;
mod link_port_markers;
mod link_port_ref;
mod node;
pub mod query;
mod validation;

// Internal types - only accessible within the crate
pub(crate) use internal::{GraphChecksum, InternalProcessorLinkGraph};

// Re-export for testing (used by integration tests)
#[doc(hidden)]
pub use internal::processor_link_graph::compute_config_checksum;

// Re-export all public types
pub use components::{
    EcsComponentJson, LightweightMarker, LinkOutputToProcessorWriterAndReader, LinkStateComponent,
    MainThreadMarker, PendingDeletion, ProcessorInstance, ProcessorMetrics, ProcessorPauseGate,
    RayonPoolMarker, ShutdownChannel, StateComponent, ThreadHandle,
};
pub use link::{Link, LinkDirection, LinkEndpoint, LinkState};
pub use link_port_markers::{input, output, InputPortMarker, OutputPortMarker, PortMarker};
pub use link_port_ref::{IntoLinkPortRef, LinkPortRef};
pub use node::{NodePorts, PortInfo, PortKind, ProcessorId, ProcessorNode};

// Public API - Graph is the unified interface
pub use graph::{Graph, GraphState};

// Query interface
pub use query::{
    FieldResolver, GraphQueryExecutor, GraphQueryInterface, LinkQuery, LinkQueryBuilder,
    LinkQueryResult, ProcessorQuery, ProcessorQueryBuilder, ProcessorQueryResult, Query,
    QueryBuilder,
};
