// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::NodeRunId;

use super::super::{InputLinkPortRef, OutputLinkPortRef};

/// Marker trait for output ports.
pub trait OutputPortMarker {
    const PORT_NAME: &'static str;
    type Node;
}

/// Marker trait for input ports.
pub trait InputPortMarker {
    const PORT_NAME: &'static str;
    type Node;
}

/// Create an [`OutputLinkPortRef`] using compile-time validated marker types.
pub fn output<M: OutputPortMarker>(node_run_id: &NodeRunId) -> OutputLinkPortRef {
    OutputLinkPortRef::new(node_run_id.clone(), M::PORT_NAME)
}

/// Create an [`InputLinkPortRef`] using compile-time validated marker types.
pub fn input<M: InputPortMarker>(node_run_id: &NodeRunId) -> InputLinkPortRef {
    InputLinkPortRef::new(node_run_id.clone(), M::PORT_NAME)
}

/// Wrapper trait for port markers.
pub trait PortMarker {
    const PORT_NAME: &'static str;
}

impl<M: OutputPortMarker> PortMarker for M {
    const PORT_NAME: &'static str = M::PORT_NAME;
}
