use std::ops::ControlFlow;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use crate::core::compiler::delta::GraphDelta;
use crate::core::compiler::{shutdown_all_processors, shutdown_processor, Compiler};
use crate::core::context::RuntimeContext;
use crate::core::delegates::{
    DefaultFactory, FactoryDelegate, ProcessorDelegate, SchedulerDelegate,
};
use crate::core::graph::{
    GraphState, IntoLinkPortRef, Link, ProcessorId, ProcessorNode, PropertyGraph,
};
use crate::core::link_channel::{LinkChannel, LinkId};
use crate::core::processors::Processor;
use crate::core::Result;

/// Runtime status information.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStatus {
    pub running: bool,
    pub processor_count: usize,
    pub link_count: usize,
    pub processor_states: Vec<(ProcessorId, String)>,
}

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
    /// Unified graph with topology and ECS components.
    pub(crate) graph: Arc<RwLock<PropertyGraph>>,
    /// Compiles graph changes into running processors.
    pub(crate) compiler: Compiler,
    /// Concrete factory for processor registration.
    pub(crate) default_factory: Arc<DefaultFactory>,
    /// Factory delegate for processor creation (same as default_factory but dyn).
    #[allow(dead_code)]
    pub(crate) factory: Arc<dyn FactoryDelegate>,
    /// Processor lifecycle delegate.
    #[allow(dead_code)]
    pub(crate) processor_delegate: Arc<dyn ProcessorDelegate>,
    /// Scheduler delegate for thread decisions.
    #[allow(dead_code)]
    pub(crate) scheduler: Arc<dyn SchedulerDelegate>,
    /// When mutations are applied.
    pub(crate) commit_mode: CommitMode,
    /// Link channel for ring buffers.
    pub(crate) link_channel: LinkChannel,
    /// Runtime context (GPU, audio config).
    pub(crate) runtime_context: Option<Arc<RuntimeContext>>,
    /// Tracks pending changes since last compile.
    pub(crate) pending_delta: GraphDelta,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        use crate::core::delegates::{DefaultProcessorDelegate, DefaultScheduler};
        use crate::core::graph::Graph;

        let default_factory = Arc::new(DefaultFactory::new());
        let factory: Arc<dyn FactoryDelegate> =
            Arc::clone(&default_factory) as Arc<dyn FactoryDelegate>;
        let processor_delegate: Arc<dyn ProcessorDelegate> = Arc::new(DefaultProcessorDelegate);
        let scheduler: Arc<dyn SchedulerDelegate> = Arc::new(DefaultScheduler);

        let compiler = Compiler::with_delegates(
            Arc::clone(&factory),
            Arc::clone(&processor_delegate),
            Arc::clone(&scheduler),
        );

        let graph = Arc::new(RwLock::new(Graph::new()));
        let property_graph = Arc::new(RwLock::new(PropertyGraph::new(graph)));

        Self {
            graph: property_graph,
            compiler,
            default_factory,
            factory,
            processor_delegate,
            scheduler,
            commit_mode: CommitMode::Auto,
            link_channel: LinkChannel::new(),
            runtime_context: None,
            pending_delta: GraphDelta::default(),
        }
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn builder() -> crate::core::runtime::RuntimeBuilder {
        crate::core::runtime::RuntimeBuilder::new()
    }

    /// Create a runtime with a specific commit mode.
    pub fn with_commit_mode(mode: CommitMode) -> Self {
        Self::builder().with_commit_mode(mode).build()
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

    pub fn graph(&self) -> &Arc<RwLock<PropertyGraph>> {
        &self.graph
    }

    // =========================================================================
    // Commit Control
    // =========================================================================

    /// Apply all pending graph changes.
    ///
    /// In `Auto` mode, this is called automatically after each mutation.
    /// In `Manual` mode, call this explicitly to batch changes.
    pub fn commit(&mut self) -> Result<()> {
        // Only compile if there are pending changes
        if self.pending_delta.is_empty() {
            return Ok(());
        }

        // Ensure runtime context exists
        if self.runtime_context.is_none() {
            use crate::core::context::GpuContext;
            let gpu = GpuContext::init_for_platform_sync()?;
            self.runtime_context = Some(Arc::new(RuntimeContext::new(gpu)));
        }

        let runtime_ctx = self.runtime_context.as_ref().unwrap();
        let delta = std::mem::take(&mut self.pending_delta);

        // Compile the delta
        let mut property_graph = self.graph.write();
        self.compiler.compile(
            &mut property_graph,
            runtime_ctx,
            &mut self.link_channel,
            &delta,
        )?;

        Ok(())
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
        self.default_factory.register::<P>();

        // Add to underlying graph
        let node = {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.add_processor_node::<P>(config)?
        };

        // Track in pending delta
        self.pending_delta.processors_to_add.push(node.id.clone());

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidAddProcessor {
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
        // Add to underlying graph
        let link = {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.add_link(from, to)?
        };

        // Track in pending delta
        self.pending_delta.links_to_add.push(link.id.clone());

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidCreateLink {
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
        let link_id = link.id.clone();

        // Unwire if already wired
        {
            let mut property_graph = self.graph.write();
            let _ = crate::core::compiler::wiring::unwire_link(&mut property_graph, &link_id);
        }

        // Remove from underlying graph
        {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.remove_link(&link_id);
        }

        // Track in pending delta
        self.pending_delta.links_to_remove.push(link_id);

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidRemoveLink {
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
            let property_graph = self.graph.read();
            if let Some(link) = property_graph.get_link(link_id) {
                (link.from_port(), link.to_port())
            } else {
                (String::new(), String::new())
            }
        };

        // Unwire if already wired
        {
            let mut property_graph = self.graph.write();
            let _ = crate::core::compiler::wiring::unwire_link(&mut property_graph, link_id);
        }

        // Remove from underlying graph
        {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.remove_link(link_id);
        }

        // Track in pending delta
        self.pending_delta.links_to_remove.push(link_id.clone());

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidRemoveLink {
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
        // Shutdown processor if running
        {
            let mut property_graph = self.graph.write();
            let _ = shutdown_processor(&mut property_graph, processor_id);
            property_graph.remove_processor_entity(processor_id);
        }

        // Remove from underlying graph
        {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.remove_processor_node(processor_id);
        }

        // Track in pending delta
        self.pending_delta
            .processors_to_remove
            .push(processor_id.clone());

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidRemoveProcessor {
                processor_id: processor_id.clone(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
    }

    /// Update a processor's configuration at runtime.
    pub fn update_processor_config<C: Serialize>(
        &mut self,
        processor_id: &ProcessorId,
        config: C,
    ) -> Result<()> {
        let config_json = serde_json::to_value(&config)
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;

        {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.update_processor_config(processor_id, config_json)?;
        }

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorConfigDidChange {
                processor_id: processor_id.clone(),
            }),
        );

        // Handle commit mode
        self.on_graph_changed()
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the runtime.
    pub fn start(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        // Set graph state to Running
        self.graph.write().set_state(GraphState::Running);

        // Compile any pending changes (this also initializes RuntimeContext if needed)
        self.commit()?;

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
        );

        Ok(())
    }

    /// Stop the runtime.
    pub fn stop(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping),
        );

        // Shutdown all processors
        {
            let mut property_graph = self.graph.write();
            shutdown_all_processors(&mut property_graph)?;
        }

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
        );

        Ok(())
    }

    /// Pause the runtime.
    pub fn pause(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePausing),
        );

        // Set graph state to Paused
        self.graph.write().set_state(GraphState::Paused);

        // TODO: Signal processors to pause (they should check state periodically)

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePaused),
        );

        Ok(())
    }

    /// Resume the runtime.
    pub fn resume(&mut self) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResuming),
        );

        // Set graph state to Running
        self.graph.write().set_state(GraphState::Running);

        // TODO: Signal processors to resume

        PUBSUB.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumed),
        );

        Ok(())
    }

    /// Block until shutdown signal (Ctrl+C, SIGTERM, Cmd+Q).
    pub fn wait_for_signal(&mut self) -> Result<()> {
        self.wait_for_signal_with(|_| ControlFlow::Continue(()))
    }

    /// Block until shutdown signal, with periodic callback for dynamic control.
    #[allow(unused_variables, unused_mut)]
    pub fn wait_for_signal_with<F>(&mut self, mut callback: F) -> Result<()>
    where
        F: FnMut(&mut Self) -> ControlFlow<()>,
    {
        use crate::core::pubsub::{topics, Event, EventListener, RuntimeEvent, PUBSUB};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // Install signal handlers
        crate::core::signals::install_signal_handlers().map_err(|e| {
            crate::core::StreamError::Configuration(format!(
                "Failed to install signal handlers: {}",
                e
            ))
        })?;

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = Arc::clone(&shutdown_flag);

        // Listener that sets shutdown flag when RuntimeShutdown received
        struct ShutdownListener {
            flag: Arc<AtomicBool>,
        }

        impl EventListener for ShutdownListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) = event {
                    self.flag.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }

        let listener = ShutdownListener {
            flag: shutdown_flag_clone.clone(),
        };
        PUBSUB.subscribe(
            topics::RUNTIME_GLOBAL,
            Arc::new(parking_lot::Mutex::new(listener)),
        );

        // On macOS, run the NSApplication event loop (required for GUI)
        #[cfg(target_os = "macos")]
        {
            crate::apple::runtime_ext::run_macos_event_loop();
            // Event loop exited (Cmd+Q or terminate)
            self.stop()?;
            Ok(())
        }

        // Non-macOS: poll loop
        #[cfg(not(target_os = "macos"))]
        {
            while !shutdown_flag.load(Ordering::SeqCst) {
                // Call user callback
                if let ControlFlow::Break(()) = callback(self) {
                    break;
                }

                // Small sleep to avoid busy-waiting
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // Auto-stop on exit
            self.stop()?;

            Ok(())
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        let property_graph = self.graph.read();
        let graph = property_graph.graph().read();

        RuntimeStatus {
            running: property_graph.state() == GraphState::Running,
            processor_count: graph.processor_count(),
            link_count: graph.link_count(),
            processor_states: vec![], // TODO: Implement processor state tracking
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let _runtime = StreamRuntime::new();
        // Runtime starts in Idle state
    }

    #[test]
    fn test_runtime_default_is_auto() {
        let runtime = StreamRuntime::new();
        assert_eq!(runtime.commit_mode(), CommitMode::Auto);
    }

    #[test]
    fn test_runtime_builder() {
        let runtime = StreamRuntime::builder()
            .with_commit_mode(CommitMode::Manual)
            .build();
        assert_eq!(runtime.commit_mode(), CommitMode::Manual);
    }
}
