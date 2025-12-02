//! Builder pattern for StreamRuntime configuration.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::delegates::{
    DefaultFactory, DefaultProcessorDelegate, DefaultScheduler, FactoryDelegate, ProcessorDelegate,
    SchedulerDelegate,
};
use crate::core::executor::SimpleExecutor;
use crate::core::graph::Graph;
use crate::core::processors::RegistryBackedFactory;

use super::{CommitMode, StreamRuntime};

/// Builder for configuring and constructing a [`StreamRuntime`].
pub struct RuntimeBuilder {
    factory: Option<Arc<dyn FactoryDelegate>>,
    processor_delegate: Option<Arc<dyn ProcessorDelegate>>,
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
            factory: None,
            processor_delegate: None,
            scheduler: None,
            commit_mode: CommitMode::Auto,
        }
    }

    /// Set a custom factory delegate.
    pub fn with_factory<F: FactoryDelegate + 'static>(mut self, factory: F) -> Self {
        self.factory = Some(Arc::new(factory));
        self
    }

    /// Set a custom factory delegate from an Arc.
    pub fn with_factory_arc(mut self, factory: Arc<dyn FactoryDelegate>) -> Self {
        self.factory = Some(factory);
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
        let graph = Arc::new(RwLock::new(Graph::new()));

        // Use provided delegates or defaults
        let _factory_delegate = self
            .factory
            .unwrap_or_else(|| Arc::new(DefaultFactory::new()));
        let _processor_delegate = self
            .processor_delegate
            .unwrap_or_else(|| Arc::new(DefaultProcessorDelegate));
        let _scheduler = self.scheduler.unwrap_or_else(|| Arc::new(DefaultScheduler));

        // For now, we still need to use RegistryBackedFactory for the executor
        // because SimpleExecutor expects ProcessorNodeFactory.
        // TODO: Update SimpleExecutor to use delegates in Phase 3
        let factory = Arc::new(RegistryBackedFactory::new());
        let executor = SimpleExecutor::with_graph_and_factory(
            Arc::clone(&graph),
            Arc::clone(&factory) as Arc<dyn crate::core::processors::factory::ProcessorNodeFactory>,
        );

        let executor = Arc::new(Mutex::new(executor));

        // Set global executor reference for event-driven callbacks
        SimpleExecutor::set_executor_ref(Arc::clone(&executor));

        StreamRuntime {
            graph,
            executor,
            factory,
            commit_mode: self.commit_mode,
        }
    }
}

impl StreamRuntime {
    /// Create a runtime builder for customization.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_default() {
        let runtime = RuntimeBuilder::new().build();
        assert_eq!(runtime.commit_mode(), CommitMode::Auto);
    }

    #[test]
    fn test_builder_with_commit_mode() {
        let runtime = RuntimeBuilder::new()
            .with_commit_mode(CommitMode::Manual)
            .build();
        assert_eq!(runtime.commit_mode(), CommitMode::Manual);
    }

    #[test]
    fn test_builder_via_runtime() {
        let runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::Manual)
            .build();
        assert_eq!(runtime.commit_mode(), CommitMode::Manual);
    }

    #[test]
    fn test_builder_with_default_delegates() {
        let runtime = RuntimeBuilder::new()
            .with_factory(DefaultFactory::new())
            .with_processor_delegate(DefaultProcessorDelegate)
            .with_scheduler(DefaultScheduler)
            .build();
        assert_eq!(runtime.commit_mode(), CommitMode::Auto);
    }
}
