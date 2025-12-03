//! Processor delegate trait for lifecycle callbacks.

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::graph::{ProcessorId, ProcessorNode};
use crate::core::processors::BoxedProcessor;

/// Delegate for processor lifecycle events.
///
/// Provides hooks for observing and customizing processor lifecycle:
/// - Creation: will_create, did_create
/// - Starting: will_start, did_start
/// - Stopping: will_stop, did_stop
/// - Config updates: did_update_config
///
/// All methods have default no-op implementations, so you only need
/// to override the ones you care about.
///
/// A blanket implementation is provided for `Arc<dyn ProcessorDelegate>`,
/// so you can pass an Arc directly where a `ProcessorDelegate` is expected.
pub trait ProcessorDelegate: Send + Sync {
    /// Called before a processor is created.
    fn will_create(&self, _node: &ProcessorNode) -> Result<()> {
        Ok(())
    }

    /// Called after a processor is created successfully.
    fn did_create(&self, _node: &ProcessorNode, _processor: &BoxedProcessor) -> Result<()> {
        Ok(())
    }

    /// Called before a processor starts.
    fn will_start(&self, _id: &ProcessorId) -> Result<()> {
        Ok(())
    }

    /// Called after a processor starts successfully.
    fn did_start(&self, _id: &ProcessorId) -> Result<()> {
        Ok(())
    }

    /// Called before a processor stops.
    fn will_stop(&self, _id: &ProcessorId) -> Result<()> {
        Ok(())
    }

    /// Called after a processor stops.
    fn did_stop(&self, _id: &ProcessorId) -> Result<()> {
        Ok(())
    }

    /// Called when a processor's config is updated.
    fn did_update_config(&self, _id: &ProcessorId, _config: &serde_json::Value) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// Blanket implementation for Arc wrapper
// =============================================================================

impl ProcessorDelegate for Arc<dyn ProcessorDelegate> {
    fn will_create(&self, node: &ProcessorNode) -> Result<()> {
        (**self).will_create(node)
    }

    fn did_create(&self, node: &ProcessorNode, processor: &BoxedProcessor) -> Result<()> {
        (**self).did_create(node, processor)
    }

    fn will_start(&self, id: &ProcessorId) -> Result<()> {
        (**self).will_start(id)
    }

    fn did_start(&self, id: &ProcessorId) -> Result<()> {
        (**self).did_start(id)
    }

    fn will_stop(&self, id: &ProcessorId) -> Result<()> {
        (**self).will_stop(id)
    }

    fn did_stop(&self, id: &ProcessorId) -> Result<()> {
        (**self).did_stop(id)
    }

    fn did_update_config(&self, id: &ProcessorId, config: &serde_json::Value) -> Result<()> {
        (**self).did_update_config(id, config)
    }
}
