//! Core compiler struct that orchestrates graph compilation.
//!
//! The Compiler is responsible for the 4-phase compilation pipeline:
//! 1. CREATE - Instantiate processors via factory
//! 2. WIRE - Create ring buffers and connect ports
//! 3. SETUP - Initialize processors (GPU, devices)
//! 4. START - Spawn processor threads

use std::sync::Arc;

use crate::core::compiler::delta::GraphDelta;
use crate::core::context::RuntimeContext;
use crate::core::delegates::{
    DefaultProcessorDelegate, DefaultScheduler, FactoryDelegate, ProcessorDelegate,
    SchedulerDelegate,
};
use crate::core::error::Result;
use crate::core::graph::PropertyGraph;
use crate::core::link_channel::LinkChannel;
use crate::core::processors::ProcessorNodeFactory;

/// Compiles graph changes into running processor state.
pub struct Compiler {
    factory: Arc<dyn FactoryDelegate>,
    processor_delegate: Arc<dyn ProcessorDelegate>,
    scheduler: Arc<dyn SchedulerDelegate>,
}

impl Compiler {
    /// Create a new compiler with the given factory.
    pub fn new(factory: Arc<dyn ProcessorNodeFactory>) -> Self {
        Self {
            factory: Arc::new(factory) as Arc<dyn FactoryDelegate>,
            processor_delegate: Arc::new(DefaultProcessorDelegate),
            scheduler: Arc::new(DefaultScheduler),
        }
    }

    /// Create a new compiler with full delegate configuration.
    pub fn with_delegates(
        factory: Arc<dyn FactoryDelegate>,
        processor_delegate: Arc<dyn ProcessorDelegate>,
        scheduler: Arc<dyn SchedulerDelegate>,
    ) -> Self {
        Self {
            factory,
            processor_delegate,
            scheduler,
        }
    }

    /// Get a reference to the factory delegate.
    pub fn factory(&self) -> &Arc<dyn FactoryDelegate> {
        &self.factory
    }

    /// Get a reference to the processor delegate.
    pub fn processor_delegate(&self) -> &Arc<dyn ProcessorDelegate> {
        &self.processor_delegate
    }

    /// Get a reference to the scheduler delegate.
    pub fn scheduler(&self) -> &Arc<dyn SchedulerDelegate> {
        &self.scheduler
    }

    /// Compile graph changes.
    ///
    /// Executes 4 phases: Create, Wire, Setup, Start.
    /// Only processes the delta (changes since last compilation).
    pub fn compile(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        link_channel: &mut LinkChannel,
        delta: &GraphDelta,
    ) -> Result<()> {
        tracing::info!(
            "Compiling: {} processors to add, {} links to add",
            delta.processors_to_add.len(),
            delta.links_to_add.len()
        );

        // Phase 1: Create processor instances
        super::phases::phase_create(
            &self.factory,
            &self.processor_delegate,
            property_graph,
            delta,
        )?;

        // Phase 2: Wire links (create ring buffers, connect ports)
        super::phases::phase_wire(property_graph, link_channel, delta)?;

        // Phase 3: Setup processors (GPU init, device open)
        super::phases::phase_setup(property_graph, runtime_context, delta)?;

        // Phase 4: Start processor threads
        super::phases::phase_start(
            &self.processor_delegate,
            &self.scheduler,
            property_graph,
            delta,
        )?;

        // Mark the graph as compiled
        property_graph.mark_compiled();

        tracing::info!("Compile complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::processors::CompositeFactory;

    #[test]
    fn test_compiler_creation() {
        let factory = Arc::new(CompositeFactory::new());
        let compiler = Compiler::new(factory);
        assert!(!compiler.factory().can_create("unknown"));
    }

    #[test]
    fn test_compiler_with_delegates() {
        use crate::core::delegates::DefaultFactory;

        let factory = Arc::new(DefaultFactory::new());
        let processor_delegate = Arc::new(DefaultProcessorDelegate);
        let scheduler = Arc::new(DefaultScheduler);

        let compiler = Compiler::with_delegates(factory, processor_delegate, scheduler);
        assert!(!compiler.factory().can_create("unknown"));
    }
}
