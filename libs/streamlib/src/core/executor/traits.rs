use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::graph::Graph;

use super::ExecutorState;

/// Executor trait for running processor graphs.
///
/// Lifecycle methods (`start`, `stop`, `pause`, `resume`) are idempotent -
/// calling them when already in the target state returns `Ok(())`.
pub trait Executor: Send {
    /// Current executor state.
    fn state(&self) -> ExecutorState;

    /// Compile the graph into an execution plan.
    fn compile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    /// Recompile after graph changes.
    fn recompile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    /// Start the executor - spawns processors and wires links.
    /// Idempotent: returns Ok if already running.
    fn start(&mut self) -> Result<()>;

    /// Stop the executor - shuts down all processors.
    /// Idempotent: returns Ok if already stopped.
    fn stop(&mut self) -> Result<()>;

    /// Pause execution - suspends processing but keeps state.
    /// Idempotent: returns Ok if already paused.
    fn pause(&mut self) -> Result<()>;

    /// Resume from paused state.
    /// Idempotent: returns Ok if already running.
    fn resume(&mut self) -> Result<()>;

    /// Block until shutdown signal (Ctrl+C / SIGTERM).
    /// Call after `start()` to keep the process alive.
    fn block_until_signal(&self) -> Result<()>;

    /// Check if graph has changed and needs recompilation.
    fn needs_recompile(&self) -> bool;
}
