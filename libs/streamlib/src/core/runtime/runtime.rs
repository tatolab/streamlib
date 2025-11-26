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

/// The main stream processing runtime.
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

    /// Add a processor to the graph with its config.
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

    /// Connect two ports - adds a link to the graph.
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

    pub fn disconnect(&mut self, link: &Link) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_link(&link.id);
        Ok(())
    }

    pub fn disconnect_by_id(&mut self, link_id: &LinkId) -> Result<()> {
        let mut graph = self.graph.write();
        graph.remove_link(link_id);
        Ok(())
    }

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

    /// Start the runtime.
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

    /// Stop the runtime.
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

    /// Pause the runtime.
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

    /// Resume the runtime.
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

    /// Run the runtime (blocking until shutdown signal).
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
