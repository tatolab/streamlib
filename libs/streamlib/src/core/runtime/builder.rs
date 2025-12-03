//! Builder pattern for StreamRuntime configuration.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::compiler::delta::GraphDelta;
use crate::core::compiler::Compiler;
use crate::core::delegates::{FactoryDelegate, ProcessorDelegate, SchedulerDelegate};
use crate::core::graph::{Graph, PropertyGraph};

use super::delegates::{DefaultFactory, DefaultProcessorDelegate, DefaultScheduler};
use super::{CommitMode, StreamRuntime};

/// Builder for configuring and constructing a [`StreamRuntime`].
pub struct RuntimeBuilder {
    default_factory: Option<Arc<DefaultFactory>>,
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
            default_factory: None,
            processor_delegate: None,
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
        let scheduler = self.scheduler.unwrap_or_else(|| Arc::new(DefaultScheduler));

        // Create compiler with delegates
        let compiler = Compiler::from_arcs(
            Arc::clone(&factory),
            Arc::clone(&processor_delegate),
            Arc::clone(&scheduler),
        );

        // Create graph and property graph
        let graph = Arc::new(RwLock::new(Graph::new()));
        let property_graph = Arc::new(RwLock::new(PropertyGraph::new(graph)));

        StreamRuntime {
            graph: property_graph,
            compiler,
            default_factory,
            factory,
            processor_delegate,
            scheduler,
            commit_mode: self.commit_mode,
            runtime_context: None,
            pending_delta: GraphDelta::default(),
            started: false,
        }
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
