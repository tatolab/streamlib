//! Core compiler struct that orchestrates graph compilation.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::executor::delta::GraphDelta;
use crate::core::executor::execution_graph::ExecutionGraph;
use crate::core::graph::Graph;
use crate::core::link_channel::LinkChannel;
use crate::core::processors::ProcessorNodeFactory;

/// Compiles graph changes into running processor state.
///
/// The Compiler is responsible for the 4-phase compilation pipeline:
/// 1. CREATE - Instantiate processors via factory
/// 2. WIRE - Create ring buffers and connect ports
/// 3. SETUP - Initialize processors (GPU, devices)
/// 4. START - Spawn processor threads
pub struct Compiler {
    factory: Arc<dyn ProcessorNodeFactory>,
}

impl Compiler {
    /// Create a new compiler with the given factory.
    pub fn new(factory: Arc<dyn ProcessorNodeFactory>) -> Self {
        Self { factory }
    }

    /// Get a reference to the factory.
    pub fn factory(&self) -> &Arc<dyn ProcessorNodeFactory> {
        &self.factory
    }

    /// Compile graph changes.
    ///
    /// Executes 4 phases: Create, Wire, Setup, Start.
    /// Only processes the delta (changes since last compilation).
    pub(crate) fn compile(
        &self,
        graph: &Arc<RwLock<Graph>>,
        execution_graph: &mut ExecutionGraph,
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
        super::phases::phase_create(&self.factory, graph, execution_graph, delta)?;

        // Phase 2: Wire links (create ring buffers, connect ports)
        super::phases::phase_wire(graph, execution_graph, link_channel, delta)?;

        // Phase 3: Setup processors (GPU init, device open)
        super::phases::phase_setup(execution_graph, runtime_context, delta)?;

        // Phase 4: Start processor threads
        super::phases::phase_start(execution_graph, delta)?;

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
        assert!(compiler.factory().can_create("unknown") == false);
    }
}
