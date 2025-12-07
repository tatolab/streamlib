// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
#[allow(clippy::module_inception)]
mod graph;
mod internal;

mod edges;
mod nodes;
mod query;
mod traits;
mod validation;

pub use traits::{GraphEdge, GraphNode, GraphWeight};

// Public API - Graph is the unified interface
pub use graph::{Graph, GraphState};

// Query interface
pub use query::{
    LinkQuery, LinkQueryMut, ProcessorQuery, ProcessorQueryMut, QueryBuilder, QueryBuilderMut,
};

pub use edges::{
    input, output, InputPortMarker, IntoLinkPortRef, Link, LinkDirection, LinkEndpoint,
    LinkPortRef, LinkState, OutputPortMarker, PortMarker,
};

pub use nodes::{NodePorts, PortInfo, PortKind, ProcessorId, ProcessorNode};

pub use components::{
    JsonComponent, LightweightMarker, LinkOutputToProcessorWriterAndReader, LinkStateComponent,
    MainThreadMarkerComponent, PendingDeletionComponent, ProcessorInstanceComponent,
    ProcessorMetrics, ProcessorPauseGateComponent, RayonPoolMarkerComponent,
    ShutdownChannelComponent, StateComponent, ThreadHandleComponent,
};
