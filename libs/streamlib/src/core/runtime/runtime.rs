use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use crate::core::executor::{Executor, SimpleExecutor};
use crate::core::graph::{Graph, IntoLinkPortRef, Link, ProcessorId, ProcessorNode};
use crate::core::link_channel::LinkId;
use crate::core::processors::Processor;
use crate::core::Result;

// Re-export types
pub use crate::core::executor::RuntimeStatus;

/// The main stream processing runtime
///
/// A thin orchestrator that ONLY modifies the Graph and publishes events.
/// All lifecycle, state management, and execution is delegated to the Executor.
pub struct StreamRuntime {
    graph: Arc<RwLock<Graph>>,
    executor: SimpleExecutor,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let executor = SimpleExecutor::with_graph(Arc::clone(&graph));

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

    /// Add a processor to the graph with its config
    ///
    /// Thin proxy to `Graph::add_processor_node`. All introspection and metadata
    /// extraction happens in the graph layer.
    ///
    /// Returns the `ProcessorNode` (pure serializable data).
    ///
    /// # Example
    /// ```ignore
    /// let camera = runtime.add_processor::<CameraProcessor>(CameraConfig {
    ///     device_id: None,
    /// })?;
    /// ```
    pub fn add_processor<P>(&mut self, config: P::Config) -> Result<ProcessorNode>
    where
        P: Processor + 'static,
        P::Config: Serialize,
    {
        // Delegate to graph layer
        let node = {
            let mut graph = self.graph.write();
            graph.add_processor_node::<P>(config)?
        };

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorAdded {
                processor_id: node.id.clone(),
                processor_type: node.processor_type.clone(),
            }),
        );

        Ok(node)
    }

    /// Connect two ports - adds a link to the graph
    ///
    /// Accepts [`LinkPortRef`](crate::core::graph::LinkPortRef) which can be created via:
    /// - Type-safe helpers: [`output`](crate::core::graph::output) / [`input`](crate::core::graph::input)
    /// - Raw strings: `"camera_0.video"` (escape hatch)
    pub fn connect(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<Link> {
        // Delegate to graph layer
        let link = {
            let mut graph = self.graph.write();
            graph.add_link(from, to)?
        };

        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::LinkCreated {
                link_id: link.id.to_string(),
                from_port: link.from_port(),
                to_port: link.to_port(),
            }),
        );

        Ok(link)
    }

    /// Disconnect by link - removes link from graph
    pub fn disconnect(&mut self, link: &Link) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_link(&link.id);
        Ok(())
    }

    /// Disconnect by link ID
    pub fn disconnect_by_id(&mut self, link_id: &LinkId) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_link(link_id);
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
