// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::ProcessorUniqueId;

use super::super::{InputLinkPortRef, OutputLinkPortRef};

/// Marker trait for output ports.
pub trait OutputPortMarker {
    const PORT_NAME: &'static str;
    type Processor;
}

/// Marker trait for input ports.
pub trait InputPortMarker {
    const PORT_NAME: &'static str;
    type Processor;
}

/// Create an [`OutputLinkPortRef`] using compile-time validated marker types.
pub fn output<M: OutputPortMarker>(processor_id: &ProcessorUniqueId) -> OutputLinkPortRef {
    OutputLinkPortRef::new(processor_id.clone(), M::PORT_NAME)
}

/// Create an [`InputLinkPortRef`] using compile-time validated marker types.
pub fn input<M: InputPortMarker>(processor_id: &ProcessorUniqueId) -> InputLinkPortRef {
    InputLinkPortRef::new(processor_id.clone(), M::PORT_NAME)
}

/// Wrapper trait for port markers.
pub trait PortMarker {
    const PORT_NAME: &'static str;
}

impl<M: OutputPortMarker> PortMarker for M {
    const PORT_NAME: &'static str = M::PORT_NAME;
}
