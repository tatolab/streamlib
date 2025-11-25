//! Stream processing runtime
//!
//! # Design (LOCKED)
//!
//! The `StreamRuntime` is a **thin orchestrator** with exactly three responsibilities:
//!
//! 1. **Graph Mutations** - Add/remove processors and connections (modifies the DOM)
//! 2. **Event Publishing** - Publishes lifecycle events to EVENT_BUS
//! 3. **Executor Delegation** - Delegates all execution to the Executor
//!
//! ## What Runtime Does
//!
//! - Owns the `Graph` (shared via `Arc<RwLock<Graph>>`)
//! - Owns the `Executor` (currently `LegacyExecutor`)
//! - Publishes `RuntimeEvent` variants for all lifecycle transitions
//! - Provides public API for graph manipulation and lifecycle control
//!
//! ## What Runtime Does NOT Do
//!
//! - ❌ Manage processor instances (Executor's job)
//! - ❌ Manage connections/wiring (Executor's job)
//! - ❌ Handle threading/scheduling (Executor's job)
//! - ❌ Track execution state (Executor's job)
//! - ❌ Run event loops (Executor's job)
//!
//! ## Event Publishing Pattern
//!
//! All lifecycle methods follow this pattern:
//! ```text
//! 1. Publish "Starting/Stopping/Pausing/Resuming" event
//! 2. Delegate to executor
//! 3. Publish "Started/Stopped/Paused/Resumed" or "Failed" event based on Result
//! ```
//!
//! This ensures consistent event publishing regardless of executor implementation.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::bus::ConnectionId;
use crate::core::executor::{Executor, LegacyExecutor};
use crate::core::graph::{ConnectionEdge, Graph, ProcessorId, ProcessorNode};
use crate::core::Result;

// Re-export types
pub use crate::core::executor::RuntimeStatus;

/// The main stream processing runtime
///
/// A thin orchestrator that ONLY modifies the Graph and publishes events.
/// All lifecycle, state management, and execution is delegated to the Executor.
pub struct StreamRuntime {
    graph: Arc<RwLock<Graph>>,
    executor: LegacyExecutor,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let executor = LegacyExecutor::with_graph(Arc::clone(&graph));

        Self { graph, executor }
    }

    // =========================================================================
    // Graph Access
    // =========================================================================

    pub fn graph(&self) -> &Arc<RwLock<Graph>> {
        &self.graph
    }

    // =========================================================================
    // Graph Mutations
    // =========================================================================

    /// Add a processor node to the graph
    ///
    /// Returns the ProcessorNode (pure data). The executor will convert
    /// this to an actual processor instance during compile.
    pub fn add_processor(&mut self, processor_type: &str) -> Result<ProcessorNode> {
        let node = {
            let mut graph = self.graph.write();
            graph.add_processor_node(processor_type)
        };

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorAdded {
                processor_id: node.id.clone(),
                processor_type: processor_type.to_string(),
            }),
        );

        Ok(node)
    }

    /// Connect two ports - adds an edge to the graph
    ///
    /// Port addresses should be in format "processor_id.port_name".
    /// Returns the ConnectionEdge (pure data).
    pub fn connect(&mut self, from_port: &str, to_port: &str) -> Result<ConnectionEdge> {
        let edge = {
            let mut graph = self.graph.write();
            graph.add_connection_edge(from_port, to_port)
        };

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ConnectionCreated {
                connection_id: edge.id.to_string(),
                from_port: from_port.to_string(),
                to_port: to_port.to_string(),
            }),
        );

        Ok(edge)
    }

    /// Disconnect by edge - removes edge from graph
    pub fn disconnect(&mut self, edge: &ConnectionEdge) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_connection_edge(&edge.id);
        Ok(())
    }

    /// Disconnect by connection ID
    pub fn disconnect_by_id(&mut self, connection_id: &ConnectionId) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_connection_edge(connection_id);
        Ok(())
    }

    /// Remove a processor node from the graph
    pub fn remove_processor(&mut self, node: &ProcessorNode) -> Result<()> {
        {
            let mut graph = self.graph.write();
            graph.remove_processor_node(&node.id);
        }

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorRemoved {
                processor_id: node.id.clone(),
            }),
        );

        Ok(())
    }

    /// Remove a processor by ID
    pub fn remove_processor_by_id(&mut self, processor_id: &ProcessorId) -> Result<()> {
        {
            let mut graph = self.graph.write();
            graph.remove_processor_node(processor_id);
        }

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorRemoved {
                processor_id: processor_id.clone(),
            }),
        );

        Ok(())
    }

    // =========================================================================
    // Lifecycle - Publishes events, delegates execution to Executor
    // =========================================================================

    /// Start the runtime
    ///
    /// Publishes RuntimeStarting, then RuntimeStarted or RuntimeStartFailed.
    pub fn start(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        match self.executor.start() {
            Ok(()) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
                );
                Ok(())
            }
            Err(e) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStartFailed {
                        error: e.to_string(),
                    }),
                );
                Err(e)
            }
        }
    }

    /// Stop the runtime
    ///
    /// Publishes RuntimeStopping, then RuntimeStopped or RuntimeStopFailed.
    pub fn stop(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping),
        );

        match self.executor.stop() {
            Ok(()) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
                );
                Ok(())
            }
            Err(e) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopFailed {
                        error: e.to_string(),
                    }),
                );
                Err(e)
            }
        }
    }

    /// Pause the runtime
    ///
    /// Publishes RuntimePausing, then RuntimePaused or RuntimePauseFailed.
    pub fn pause(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePausing),
        );

        match self.executor.pause() {
            Ok(()) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimePaused),
                );
                Ok(())
            }
            Err(e) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimePauseFailed {
                        error: e.to_string(),
                    }),
                );
                Err(e)
            }
        }
    }

    /// Resume the runtime
    ///
    /// Publishes RuntimeResuming, then RuntimeResumed or RuntimeResumeFailed.
    pub fn resume(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResuming),
        );

        match self.executor.resume() {
            Ok(()) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumed),
                );
                Ok(())
            }
            Err(e) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumeFailed {
                        error: e.to_string(),
                    }),
                );
                Err(e)
            }
        }
    }

    /// Run the runtime (blocking)
    ///
    /// Starts the runtime and runs until shutdown signal received.
    /// Publishes RuntimeStarting/Started/StartFailed and RuntimeStopped/StopFailed.
    pub fn run(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        // Delegate to executor (handles start, event loop, and stop internally)
        match self.executor.run() {
            Ok(()) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
                );
                Ok(())
            }
            Err(e) => {
                EVENT_BUS.publish(
                    "runtime:global",
                    &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopFailed {
                        error: e.to_string(),
                    }),
                );
                Err(e)
            }
        }
    }

    /// Get runtime status - delegates to executor
    pub fn status(&self) -> RuntimeStatus {
        self.executor.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let _runtime = StreamRuntime::new();
        // Runtime starts in Idle state - executor manages state
    }
}
