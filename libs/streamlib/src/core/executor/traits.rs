use crate::core::error::Result;
use crate::core::graph::ProcessorId;
use crate::core::link_channel::LinkId;

use super::ExecutorState;

/// Executor lifecycle management.
///
/// Lifecycle methods (`start`, `stop`, `pause`, `resume`) are idempotent -
/// calling them when already in the target state returns `Ok(())`.
pub trait ExecutorLifecycle: Send {
    /// Current executor state.
    fn state(&self) -> ExecutorState;

    /// Start the executor.
    fn start(&mut self) -> Result<()>;

    /// Stop the executor.
    fn stop(&mut self) -> Result<()>;

    /// Pause execution.
    fn pause(&mut self) -> Result<()>;

    /// Resume from paused state.
    fn resume(&mut self) -> Result<()>;
}

/// Graph compilation operations.
///
/// Implementors translate graph definitions into running processor instances.
pub trait GraphCompiler {
    /// Compile the full graph, creating and wiring all processors.
    fn compile(&mut self) -> Result<()>;

    /// Create a processor instance from graph definition.
    fn create_processor(&mut self, processor_id: &ProcessorId) -> Result<()>;

    /// Wire a link between two processor ports.
    fn wire_link(&mut self, link_id: &LinkId) -> Result<()>;

    /// Setup a processor (call after creation and wiring).
    fn setup_processor(&mut self, processor_id: &ProcessorId) -> Result<()>;

    /// Start a processor thread.
    fn start_processor(&mut self, processor_id: &ProcessorId) -> Result<()>;

    /// Shutdown a running processor.
    fn shutdown_processor(&mut self, processor_id: &ProcessorId) -> Result<()>;
}
