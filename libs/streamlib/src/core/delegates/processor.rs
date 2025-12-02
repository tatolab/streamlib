//! Processor delegate for lifecycle callbacks.

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

/// Default implementation that does nothing.
pub struct DefaultProcessorDelegate;

impl ProcessorDelegate for DefaultProcessorDelegate {}

impl Default for DefaultProcessorDelegate {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingDelegate {
        create_count: AtomicUsize,
        start_count: AtomicUsize,
        stop_count: AtomicUsize,
    }

    impl CountingDelegate {
        fn new() -> Self {
            Self {
                create_count: AtomicUsize::new(0),
                start_count: AtomicUsize::new(0),
                stop_count: AtomicUsize::new(0),
            }
        }
    }

    impl ProcessorDelegate for CountingDelegate {
        fn will_create(&self, _node: &ProcessorNode) -> Result<()> {
            self.create_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_start(&self, _id: &ProcessorId) -> Result<()> {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_stop(&self, _id: &ProcessorId) -> Result<()> {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_default_delegate_does_nothing() {
        let delegate = DefaultProcessorDelegate;
        let node = ProcessorNode::new("test".into(), "TestProcessor".into(), None, vec![], vec![]);

        assert!(delegate.will_create(&node).is_ok());
        assert!(delegate.will_start(&"test".to_string()).is_ok());
        assert!(delegate.will_stop(&"test".to_string()).is_ok());
    }

    #[test]
    fn test_counting_delegate() {
        let delegate = Arc::new(CountingDelegate::new());
        let node = ProcessorNode::new("test".into(), "TestProcessor".into(), None, vec![], vec![]);

        delegate.will_create(&node).unwrap();
        delegate.will_create(&node).unwrap();
        delegate.will_start(&"test".to_string()).unwrap();

        assert_eq!(delegate.create_count.load(Ordering::SeqCst), 2);
        assert_eq!(delegate.start_count.load(Ordering::SeqCst), 1);
        assert_eq!(delegate.stop_count.load(Ordering::SeqCst), 0);
    }
}
