// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Default processor delegate implementation.

use crate::core::delegates::ProcessorDelegate;

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
    use crate::core::delegates::ProcessorDelegate;
    use crate::core::graph::ProcessorNode;
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
        fn will_create(&self, _node: &ProcessorNode) -> crate::core::Result<()> {
            self.create_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_start(&self, _id: &str) -> crate::core::Result<()> {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_stop(&self, _id: &str) -> crate::core::Result<()> {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_default_delegate_does_nothing() {
        let delegate = DefaultProcessorDelegate;
        let node = ProcessorNode::new("TestProcessor", None, vec![], vec![]);

        assert!(delegate.will_create(&node).is_ok());
        assert!(delegate.will_start("test").is_ok());
        assert!(delegate.will_stop("test").is_ok());
    }

    #[test]
    fn test_counting_delegate() {
        let delegate = Arc::new(CountingDelegate::new());
        let node = ProcessorNode::new("TestProcessor", None, vec![], vec![]);

        delegate.will_create(&node).unwrap();
        delegate.will_create(&node).unwrap();
        delegate.will_start("test").unwrap();

        assert_eq!(delegate.create_count.load(Ordering::SeqCst), 2);
        assert_eq!(delegate.start_count.load(Ordering::SeqCst), 1);
        assert_eq!(delegate.stop_count.load(Ordering::SeqCst), 0);
    }
}
