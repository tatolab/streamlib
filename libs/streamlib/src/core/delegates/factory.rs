// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Factory delegate trait for processor instantiation.

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{BoxedProcessor, ProcessorNodeFactory};

/// Delegate for processor instantiation.
///
/// This trait abstracts how processors are created, allowing for:
/// - Custom factory implementations
/// - Dependency injection
/// - Testing with mock processors
///
/// Blanket implementations are provided for `Arc<dyn FactoryDelegate>` and
/// `Arc<dyn ProcessorNodeFactory>`, so you can pass these directly where
/// a `FactoryDelegate` is expected.
pub trait FactoryDelegate: Send + Sync {
    /// Create a processor instance from a node definition.
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor>;

    /// Get port information for a processor type (inputs, outputs).
    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)>;

    /// Check if this factory can create a processor type.
    fn can_create(&self, processor_type: &str) -> bool;
}

// =============================================================================
// Blanket implementations for Arc wrappers
// =============================================================================

impl FactoryDelegate for Arc<dyn FactoryDelegate> {
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        (**self).create(node)
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        (**self).port_info(processor_type)
    }

    fn can_create(&self, processor_type: &str) -> bool {
        (**self).can_create(processor_type)
    }
}

impl FactoryDelegate for Arc<dyn ProcessorNodeFactory> {
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        (**self).create(node)
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        (**self).port_info(processor_type)
    }

    fn can_create(&self, processor_type: &str) -> bool {
        (**self).can_create(processor_type)
    }
}
