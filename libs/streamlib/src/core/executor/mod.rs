//! Executor module for graph execution strategies
//!
//! This module provides the abstraction layer between the graph representation (DOM)
//! and actual execution. Different executor strategies can implement different
//! execution models (thread-per-processor, thread pool, async, etc.).
//!
//! # Architecture
//!
//! ```text
//! StreamRuntime (thin orchestrator)
//!       │
//!       ├── Graph (DOM - pure data)
//!       │     • nodes (processor metadata)
//!       │     • edges (connection metadata)
//!       │     • serialize/deserialize
//!       │
//!       └── Executor (strategy pattern)
//!             • compile() - creates execution graph from DOM
//!             • start/stop/pause/resume - lifecycle control
//!             • to_processor_instance() - node → running processor
//!             • to_connection_instance() - edge → live connection
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use streamlib::core::executor::{Executor, LegacyExecutor};
//!
//! let executor = LegacyExecutor::new();
//! executor.compile(&graph)?;
//! executor.start()?;
//! // ... runtime runs ...
//! executor.stop()?;
//! ```

mod legacy;

pub use legacy::{LegacyExecutor, ProcessorStatus, RuntimeStatus};

use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::graph::Graph;

/// Execution state for the executor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorState {
    /// Not yet compiled or stopped
    Idle,
    /// Graph compiled, ready to start
    Compiled,
    /// Actively executing processors
    Running,
    /// Execution paused, can resume
    Paused,
}

/// Trait for graph execution strategies
///
/// An executor is responsible for:
/// 1. Compiling a graph into an executable form
/// 2. Managing processor instance lifecycles
/// 3. Managing connection instance lifecycles
/// 4. Controlling execution state (start/stop/pause/resume)
///
/// Different implementations can provide different execution models:
/// - `LegacyExecutor`: Thread-per-processor with lock-free ring buffers
/// - Future: Thread pool executor, async executor, distributed executor, etc.
pub trait Executor: Send {
    /// Get the current executor state
    fn state(&self) -> ExecutorState;

    /// Compile the graph into an executable form
    ///
    /// This analyzes the graph and prepares for execution by:
    /// - Validating the graph topology
    /// - Optimizing execution order
    /// - Preparing processor and connection metadata
    ///
    /// After compilation, the executor is ready to start.
    fn compile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    /// Recompile with delta changes
    ///
    /// For hot-reloading: computes the difference between the current
    /// execution state and the new graph, then applies minimal changes.
    fn recompile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    /// Start execution of the compiled graph
    ///
    /// This instantiates all processors and connections, then begins
    /// processing. Requires prior call to `compile()`.
    fn start(&mut self) -> Result<()>;

    /// Stop execution and clean up all resources
    ///
    /// This gracefully shuts down all processors and connections,
    /// releasing all resources. The executor returns to Idle state.
    fn stop(&mut self) -> Result<()>;

    /// Pause execution
    ///
    /// Processors stop processing but maintain their state.
    /// Connections remain intact. Can be resumed with `resume()`.
    fn pause(&mut self) -> Result<()>;

    /// Resume execution from paused state
    fn resume(&mut self) -> Result<()>;

    /// Run the executor (blocking)
    ///
    /// This is the main entry point for running the executor. It:
    /// 1. Compiles and starts if not already running
    /// 2. Runs the event loop (blocking until shutdown)
    /// 3. Stops and cleans up
    ///
    /// The executor handles signal handlers and event loops.
    fn run(&mut self) -> Result<()>;

    /// Check if the executor needs recompilation
    ///
    /// Returns true if the graph has changed since last compile.
    fn needs_recompile(&self) -> bool;
}
