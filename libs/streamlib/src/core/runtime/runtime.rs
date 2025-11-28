use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use crate::core::executor::{Executor, SimpleExecutor};
use crate::core::graph::{Graph, IntoLinkPortRef, Link, ProcessorId, ProcessorNode};
use crate::core::link_channel::LinkId;
use crate::core::processors::factory::RegistryBackedFactory;
use crate::core::processors::Processor;
use crate::core::Result;

// Re-export types
pub use crate::core::executor::RuntimeStatus;

/// Controls when graph mutations are applied to the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommitMode {
    /// Changes apply immediately after each mutation.
    #[default]
    Auto,
    /// Changes batch until explicit `commit()` call.
    Manual,
}

/// The main stream processing runtime.
pub struct StreamRuntime {
    graph: Arc<RwLock<Graph>>,
    executor: SimpleExecutor,
    factory: Arc<RegistryBackedFactory>,
    commit_mode: CommitMode,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        Self::with_commit_mode(CommitMode::default())
    }

    pub fn with_commit_mode(commit_mode: CommitMode) -> Self {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let factory = Arc::new(RegistryBackedFactory::new());
        let executor = SimpleExecutor::with_graph_and_factory(
            Arc::clone(&graph),
            Arc::clone(&factory) as Arc<dyn crate::core::processors::factory::ProcessorNodeFactory>,
        );

        Self {
            graph,
            executor,
            factory,
            commit_mode,
        }
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    /// Get the current commit mode.
    pub fn commit_mode(&self) -> CommitMode {
        self.commit_mode
    }

    /// Set the commit mode.
    pub fn set_commit_mode(&mut self, mode: CommitMode) {
        self.commit_mode = mode;
    }

    // =========================================================================
    // Graph Access
    // =========================================================================

    pub fn graph(&self) -> &Arc<RwLock<Graph>> {
        &self.graph
    }

    // =========================================================================
    // Commit Control
    // =========================================================================

    /// Apply all pending graph changes to the executor.
    ///
    /// In `Auto` mode, this is called automatically after each mutation.
    /// In `Manual` mode, call this explicitly to batch changes.
    pub fn commit(&mut self) -> Result<()> {
        self.executor.sync_to_graph()
    }

    /// Central handler for graph mutations - respects commit mode.
    fn on_graph_changed(&mut self) -> Result<()> {
        match self.commit_mode {
            CommitMode::Auto => self.commit(),
            CommitMode::Manual => Ok(()),
        }
    }

    // =========================================================================
    // Graph Mutations
    // =========================================================================

    /// Add a processor to the graph with its config.
    pub fn add_processor<P>(&mut self, config: P::Config) -> Result<ProcessorNode>
    where
        P: Processor + 'static,
        P::Config: Serialize + for<'de> serde::Deserialize<'de> + Default,
    {
        // Ensure type is registered with factory
        self.factory.register::<P>();

        // Add to graph
        let node = {
            let mut graph = self.graph.write();
            graph.add_processor_node::<P>(config)?
        };

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorAdded {
                processor_id: node.id.clone(),
                processor_type: node.processor_type.clone(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()?;

        Ok(node)
    }

    /// Connect two ports - adds a link to the graph.
    pub fn connect(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<Link> {
        // Add to graph
        let link = {
            let mut graph = self.graph.write();
            graph.add_link(from, to)?
        };

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::LinkCreated {
                link_id: link.id.to_string(),
                from_port: link.from_port(),
                to_port: link.to_port(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()?;

        Ok(link)
    }

    pub fn disconnect(&mut self, link: &Link) -> Result<()> {
        let from_port = link.from_port();
        let to_port = link.to_port();
        let link_id_str = link.id.to_string();

        {
            let mut graph = self.graph.write();
            graph.remove_link(&link.id);
        }

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::LinkRemoved {
                link_id: link_id_str,
                from_port,
                to_port,
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
    }

    pub fn disconnect_by_id(&mut self, link_id: &LinkId) -> Result<()> {
        // Get link info before removing
        let (from_port, to_port) = {
            let graph = self.graph.read();
            if let Some(link) = graph.get_link(link_id) {
                (link.from_port(), link.to_port())
            } else {
                (String::new(), String::new())
            }
        };

        {
            let mut graph = self.graph.write();
            graph.remove_link(link_id);
        }

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::LinkRemoved {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
    }

    pub fn remove_processor(&mut self, node: &ProcessorNode) -> Result<()> {
        self.remove_processor_by_id(&node.id)
    }

    pub fn remove_processor_by_id(&mut self, processor_id: &ProcessorId) -> Result<()> {
        {
            let mut graph = self.graph.write();
            graph.remove_processor_node(processor_id);
        }

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorRemoved {
                processor_id: processor_id.clone(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
    }

    /// Update a processor's configuration at runtime.
    ///
    /// The new config will be applied to the running processor on the next
    /// sync (immediately in Auto mode, or on explicit commit in Manual mode).
    pub fn update_processor_config<C: Serialize>(
        &mut self,
        processor_id: &ProcessorId,
        config: C,
    ) -> Result<()> {
        let config_json = serde_json::to_value(&config)
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;

        {
            let mut graph = self.graph.write();
            graph.update_processor_config(processor_id, config_json)?;
        }

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorConfigUpdated {
                processor_id: processor_id.clone(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
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

    /// Block until shutdown signal (Ctrl+C / SIGTERM).
    ///
    /// Call after `start()` to keep the process alive until interrupted.
    /// Does not automatically stop - call `stop()` after if needed.
    ///
    /// # Example
    /// ```ignore
    /// runtime.start()?;
    /// runtime.block_until_signal()?;  // Blocks here
    /// runtime.stop()?;                 // Clean shutdown
    /// ```
    pub fn block_until_signal(&self) -> Result<()> {
        self.executor.block_until_signal()
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

    #[test]
    fn test_runtime_with_manual_commit() {
        let runtime = StreamRuntime::with_commit_mode(CommitMode::Manual);
        assert_eq!(runtime.commit_mode(), CommitMode::Manual);
    }

    #[test]
    fn test_runtime_default_is_auto() {
        let runtime = StreamRuntime::new();
        assert_eq!(runtime.commit_mode(), CommitMode::Auto);
    }
}
