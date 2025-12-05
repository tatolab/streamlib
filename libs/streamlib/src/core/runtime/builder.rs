// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Builder pattern for StreamRuntime configuration.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::compiler::{Compiler, PendingOperationQueue};
use crate::core::delegates::{FactoryDelegate, LinkDelegate, ProcessorDelegate, SchedulerDelegate};
use crate::core::graph::Graph;

use crate::core::links::DefaultLinkFactory;

use super::delegates::{
    DefaultFactory, DefaultLinkDelegate, DefaultProcessorDelegate, DefaultScheduler,
};
use super::{CommitMode, StreamRuntime};

/// Builder for configuring and constructing a [`StreamRuntime`].
pub struct RuntimeBuilder {
    default_factory: Option<Arc<DefaultFactory>>,
    processor_delegate: Option<Arc<dyn ProcessorDelegate>>,
    link_delegate: Option<Arc<dyn LinkDelegate>>,
    scheduler: Option<Arc<dyn SchedulerDelegate>>,
    commit_mode: CommitMode,
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeBuilder {
    /// Create a new runtime builder with defaults.
    pub fn new() -> Self {
        Self {
            default_factory: None,
            processor_delegate: None,
            link_delegate: None,
            scheduler: None,
            commit_mode: CommitMode::Auto,
        }
    }

    /// Set a custom factory.
    pub fn with_factory(mut self, factory: DefaultFactory) -> Self {
        self.default_factory = Some(Arc::new(factory));
        self
    }

    /// Set a custom factory from an Arc.
    pub fn with_factory_arc(mut self, factory: Arc<DefaultFactory>) -> Self {
        self.default_factory = Some(factory);
        self
    }

    /// Set a custom processor delegate.
    pub fn with_processor_delegate<D: ProcessorDelegate + 'static>(mut self, delegate: D) -> Self {
        self.processor_delegate = Some(Arc::new(delegate));
        self
    }

    /// Set a custom processor delegate from an Arc.
    pub fn with_processor_delegate_arc(mut self, delegate: Arc<dyn ProcessorDelegate>) -> Self {
        self.processor_delegate = Some(delegate);
        self
    }

    /// Set a custom link delegate.
    pub fn with_link_delegate<L: LinkDelegate + 'static>(mut self, delegate: L) -> Self {
        self.link_delegate = Some(Arc::new(delegate));
        self
    }

    /// Set a custom link delegate from an Arc.
    pub fn with_link_delegate_arc(mut self, delegate: Arc<dyn LinkDelegate>) -> Self {
        self.link_delegate = Some(delegate);
        self
    }

    /// Set a custom scheduler delegate.
    pub fn with_scheduler<S: SchedulerDelegate + 'static>(mut self, scheduler: S) -> Self {
        self.scheduler = Some(Arc::new(scheduler));
        self
    }

    /// Set a custom scheduler delegate from an Arc.
    pub fn with_scheduler_arc(mut self, scheduler: Arc<dyn SchedulerDelegate>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Set the commit mode.
    pub fn with_commit_mode(mut self, mode: CommitMode) -> Self {
        self.commit_mode = mode;
        self
    }

    /// Build the runtime with the configured delegates.
    pub fn build(self) -> StreamRuntime {
        // Use provided delegates or defaults
        let default_factory = self
            .default_factory
            .unwrap_or_else(|| Arc::new(DefaultFactory::new()));
        let factory: Arc<dyn FactoryDelegate> =
            Arc::clone(&default_factory) as Arc<dyn FactoryDelegate>;
        let processor_delegate = self
            .processor_delegate
            .unwrap_or_else(|| Arc::new(DefaultProcessorDelegate));
        let link_delegate = self
            .link_delegate
            .unwrap_or_else(|| Arc::new(DefaultLinkDelegate));
        let scheduler = self.scheduler.unwrap_or_else(|| Arc::new(DefaultScheduler));

        // Create compiler with all delegates
        let compiler = Compiler::from_all_arcs(
            Arc::clone(&factory),
            Arc::clone(&processor_delegate),
            Arc::clone(&link_delegate),
            Arc::clone(&scheduler),
            Arc::new(DefaultLinkFactory),
        );

        // Create graph
        let graph = Arc::new(RwLock::new(Graph::new()));

        StreamRuntime {
            graph,
            compiler,
            default_factory,
            factory,
            processor_delegate,
            scheduler,
            commit_mode: self.commit_mode,
            runtime_context: None,
            pending_operations: PendingOperationQueue::new(),
            started: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::core::graph::Link;
    use crate::core::LinkId;

    #[test]
    fn test_builder_default() {
        let _runtime = RuntimeBuilder::new().build();
        // Default build succeeds
    }

    #[test]
    fn test_builder_with_commit_mode() {
        let _runtime = RuntimeBuilder::new()
            .with_commit_mode(CommitMode::Manual)
            .build();
        // Build with manual commit mode succeeds
    }

    #[test]
    fn test_builder_via_runtime() {
        let _runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::Manual)
            .build();
        // Build via StreamRuntime::builder() succeeds
    }

    #[test]
    fn test_builder_with_default_delegates() {
        let _runtime = RuntimeBuilder::new()
            .with_factory(DefaultFactory::new())
            .with_processor_delegate(DefaultProcessorDelegate)
            .with_scheduler(DefaultScheduler)
            .build();
        // Build with explicit default delegates succeeds
    }

    #[test]
    fn test_builder_with_link_delegate() {
        let _runtime = RuntimeBuilder::new()
            .with_link_delegate(DefaultLinkDelegate)
            .build();
        // Build with link delegate succeeds
    }

    /// A counting delegate that tracks how many times each hook is called.
    struct CountingLinkDelegate {
        will_wire_count: AtomicUsize,
        did_wire_count: AtomicUsize,
        will_unwire_count: AtomicUsize,
        did_unwire_count: AtomicUsize,
    }

    impl CountingLinkDelegate {
        fn new() -> Self {
            Self {
                will_wire_count: AtomicUsize::new(0),
                did_wire_count: AtomicUsize::new(0),
                will_unwire_count: AtomicUsize::new(0),
                did_unwire_count: AtomicUsize::new(0),
            }
        }
    }

    impl LinkDelegate for CountingLinkDelegate {
        fn will_wire(&self, _link: &Link) -> crate::core::error::Result<()> {
            self.will_wire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn did_wire(&self, _link: &Link) -> crate::core::error::Result<()> {
            self.did_wire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_unwire(&self, _link_id: &LinkId) -> crate::core::error::Result<()> {
            self.will_unwire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn did_unwire(&self, _link_id: &LinkId) -> crate::core::error::Result<()> {
            self.did_unwire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_counting_link_delegate_can_be_used() {
        // Verify CountingLinkDelegate can be used with the builder
        let delegate = Arc::new(CountingLinkDelegate::new());
        let _runtime = RuntimeBuilder::new()
            .with_link_delegate_arc(delegate)
            .build();
    }
}
