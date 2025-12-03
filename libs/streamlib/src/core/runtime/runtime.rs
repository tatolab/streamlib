use std::ops::ControlFlow;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use crate::core::compiler::{
    shutdown_all_processors, Compiler, PendingOperation, PendingOperationQueue,
};
use crate::core::context::RuntimeContext;
use crate::core::delegates::{FactoryDelegate, ProcessorDelegate, SchedulerDelegate};
use crate::core::graph::{
    GraphState, IntoLinkPortRef, Link, ProcessorId, ProcessorNode, PropertyGraph,
};
use crate::core::links::LinkId;
use crate::core::processors::Processor;
use crate::core::runtime::delegates::DefaultFactory;
use crate::core::{Result, StreamError};

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
    /// Runtime context (GPU, audio config).
    pub(crate) runtime_context: Option<Arc<RuntimeContext>>,
    /// Queue of pending operations to execute at commit time.
    pub(crate) pending_operations: PendingOperationQueue,
    /// Whether the runtime has been started.
    pub(crate) started: bool,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        use crate::core::graph::Graph;
        use crate::core::runtime::delegates::{DefaultProcessorDelegate, DefaultScheduler};

        let default_factory = Arc::new(DefaultFactory::new());
        let factory: Arc<dyn FactoryDelegate> =
            Arc::clone(&default_factory) as Arc<dyn FactoryDelegate>;
        let processor_delegate: Arc<dyn ProcessorDelegate> = Arc::new(DefaultProcessorDelegate);
        let scheduler: Arc<dyn SchedulerDelegate> = Arc::new(DefaultScheduler);

        let compiler = Compiler::from_arcs(
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
            runtime_context: None,
            pending_operations: PendingOperationQueue::new(),
            started: false,
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
    ///
    /// When the runtime is not started, pending operations are kept in the queue
    /// and will be executed when `start()` is called. When started, operations
    /// are compiled and executed with proper processor lifecycle.
    pub fn commit(&mut self) -> Result<()> {
        // Only process if there are pending changes
        if self.pending_operations.is_empty() {
            return Ok(());
        }

        // If runtime not started, operations stay queued until start()
        if !self.started {
            return Ok(());
        }

        // Take all pending operations
        let operations = self.pending_operations.take_all();

        // Runtime is started - full compilation with processor lifecycle
        let runtime_ctx = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| {
                crate::core::error::StreamError::Runtime(
                    "Runtime context not initialized".to_string(),
                )
            })?
            .clone();

        for op in operations {
            self.execute_operation(op, &runtime_ctx)?;
        }

        Ok(())
    }

    /// Execute a single pending operation with validation.
    fn execute_operation(
        &mut self,
        op: PendingOperation,
        runtime_ctx: &Arc<RuntimeContext>,
    ) -> Result<()> {
        use crate::core::compiler::GraphDelta;
        use crate::core::graph::ProcessorInstance;
        use crate::core::links::LinkInstanceComponent;

        match op {
            PendingOperation::AddProcessor(processor_id) => {
                // Validate: processor must exist in graph and NOT have ProcessorInstance
                let (exists_in_graph, already_running) = {
                    let property_graph = self.graph.read();
                    let exists = property_graph.has_processor(&processor_id);
                    let running = property_graph.has::<ProcessorInstance>(&processor_id);
                    (exists, running)
                };

                if !exists_in_graph {
                    tracing::warn!(
                        "AddProcessor({}): processor not in graph, skipping",
                        processor_id
                    );
                    return Ok(());
                }

                if already_running {
                    tracing::debug!("AddProcessor({}): already running, skipping", processor_id);
                    return Ok(());
                }

                // Create delta with just this processor
                let delta = GraphDelta {
                    processors_to_add: vec![processor_id],
                    ..Default::default()
                };

                let mut property_graph = self.graph.write();
                self.compiler
                    .compile(&mut property_graph, runtime_ctx, &delta)?;
            }

            PendingOperation::RemoveProcessor(processor_id) => {
                // Validate: processor must exist in graph OR have ProcessorInstance component
                let (exists_in_graph, has_instance) = {
                    let property_graph = self.graph.read();
                    let exists = property_graph.has_processor(&processor_id);
                    let running = property_graph.has::<ProcessorInstance>(&processor_id);
                    (exists, running)
                };

                if !exists_in_graph && !has_instance {
                    tracing::warn!(
                        "RemoveProcessor({}): not found in graph or ECS, skipping",
                        processor_id
                    );
                    return Ok(());
                }

                // First, find and queue removal of any links connected to this processor
                let connected_links: Vec<_> = {
                    let property_graph = self.graph.read();
                    let graph = property_graph.graph().read();
                    graph
                        .links()
                        .iter()
                        .filter(|link| {
                            link.source.node == processor_id || link.target.node == processor_id
                        })
                        .map(|link| link.id.clone())
                        .collect()
                };

                // Remove connected links first
                for link_id in connected_links {
                    let link_delta = GraphDelta {
                        links_to_remove: vec![link_id.clone()],
                        ..Default::default()
                    };
                    let mut property_graph = self.graph.write();
                    self.compiler
                        .compile(&mut property_graph, runtime_ctx, &link_delta)?;

                    // Remove from graph
                    let mut graph = property_graph.graph().write();
                    graph.remove_link(&link_id);
                }

                // Now remove the processor
                let delta = GraphDelta {
                    processors_to_remove: vec![processor_id.clone()],
                    ..Default::default()
                };

                {
                    let mut property_graph = self.graph.write();
                    self.compiler
                        .compile(&mut property_graph, runtime_ctx, &delta)?;

                    // Remove from graph after shutdown
                    {
                        let mut graph = property_graph.graph().write();
                        graph.remove_processor_node(&processor_id);
                    }

                    // Remove ECS entity
                    property_graph.remove_processor_entity(&processor_id);
                }

                // Publish event after successful removal
                use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::GraphDidRemoveProcessor { processor_id }),
                );
            }

            PendingOperation::AddLink(link_id) => {
                // Validate: link must exist in graph and NOT have LinkInstanceComponent
                let (exists_in_graph, already_wired) = {
                    let property_graph = self.graph.read();
                    let exists = property_graph.get_link(&link_id).is_some();
                    let wired = property_graph
                        .get_link_entity(&link_id)
                        .map(|_| {
                            property_graph
                                .get_link_component::<LinkInstanceComponent>(&link_id)
                                .is_some()
                        })
                        .unwrap_or(false);
                    (exists, wired)
                };

                if !exists_in_graph {
                    tracing::warn!("AddLink({}): link not in graph, skipping", link_id);
                    return Ok(());
                }

                if already_wired {
                    tracing::debug!("AddLink({}): already wired, skipping", link_id);
                    return Ok(());
                }

                let delta = GraphDelta {
                    links_to_add: vec![link_id],
                    ..Default::default()
                };

                let mut property_graph = self.graph.write();
                self.compiler
                    .compile(&mut property_graph, runtime_ctx, &delta)?;
            }

            PendingOperation::RemoveLink(link_id) => {
                // Validate: link must exist in graph OR have LinkInstanceComponent
                let (exists_in_graph, has_instance, from_port, to_port) = {
                    let property_graph = self.graph.read();
                    let link_info = property_graph.get_link(&link_id);
                    let exists = link_info.is_some();
                    let (from, to) = link_info
                        .map(|l| (l.from_port(), l.to_port()))
                        .unwrap_or_default();
                    let has_component = property_graph
                        .get_link_entity(&link_id)
                        .map(|_| {
                            property_graph
                                .get_link_component::<LinkInstanceComponent>(&link_id)
                                .is_some()
                        })
                        .unwrap_or(false);
                    (exists, has_component, from, to)
                };

                if !exists_in_graph && !has_instance {
                    tracing::warn!(
                        "RemoveLink({}): not found in graph or ECS, skipping",
                        link_id
                    );
                    return Ok(());
                }

                let delta = GraphDelta {
                    links_to_remove: vec![link_id.clone()],
                    ..Default::default()
                };

                {
                    let mut property_graph = self.graph.write();
                    self.compiler
                        .compile(&mut property_graph, runtime_ctx, &delta)?;

                    // Remove from graph after unwiring
                    let mut graph = property_graph.graph().write();
                    graph.remove_link(&link_id);
                }

                // Publish event after successful removal
                use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::GraphDidRemoveLink {
                        link_id: link_id.to_string(),
                        from_port,
                        to_port,
                    }),
                );
            }

            PendingOperation::UpdateProcessorConfig(processor_id) => {
                // Validate: processor must exist and be running
                let (exists_in_graph, has_instance) = {
                    let property_graph = self.graph.read();
                    let exists = property_graph.has_processor(&processor_id);
                    let running = property_graph.has::<ProcessorInstance>(&processor_id);
                    (exists, running)
                };

                if !exists_in_graph {
                    tracing::warn!(
                        "UpdateProcessorConfig({}): processor not in graph, skipping",
                        processor_id
                    );
                    return Ok(());
                }

                if !has_instance {
                    tracing::debug!(
                        "UpdateProcessorConfig({}): not running yet, config will apply on start",
                        processor_id
                    );
                    return Ok(());
                }

                // Get config from graph and apply to running processor
                let config_json = {
                    let property_graph = self.graph.read();
                    let graph = property_graph.graph().read();
                    graph
                        .get_processor(&processor_id)
                        .and_then(|node| node.config.clone())
                };

                if let Some(config) = config_json {
                    // Get the processor instance Arc, then drop the property_graph borrow
                    let processor_arc = {
                        let property_graph = self.graph.read();
                        property_graph
                            .get::<ProcessorInstance>(&processor_id)
                            .map(|instance| Arc::clone(&instance.0))
                    };

                    if let Some(processor_mutex) = processor_arc {
                        let mut processor = processor_mutex.lock();
                        if let Err(e) = processor.apply_config_json(&config) {
                            tracing::warn!(
                                "Failed to apply config to processor {}: {}",
                                processor_id,
                                e
                            );
                        }
                    }
                }
            }
        }

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

        // Queue operation for commit
        self.pending_operations
            .push(PendingOperation::AddProcessor(node.id.clone()));

        // Publish event
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
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

        // Queue operation for commit
        self.pending_operations
            .push(PendingOperation::AddLink(link.id.clone()));

        // Publish event
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
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
        self.disconnect_by_id(&link.id)
    }

    pub fn disconnect_by_id(&mut self, link_id: &LinkId) -> Result<()> {
        // Validate link exists in graph
        let link_exists = {
            let property_graph = self.graph.read();
            property_graph.get_link(link_id).is_some()
        };

        if !link_exists {
            return Err(StreamError::NotFound(format!(
                "Link '{}' not found",
                link_id
            )));
        }

        // Queue operation for commit - actual unwiring and graph removal happens during commit
        self.pending_operations
            .push(PendingOperation::RemoveLink(link_id.clone()));

        // Handle commit mode
        self.on_graph_changed()
    }

    pub fn remove_processor(&mut self, node: &ProcessorNode) -> Result<()> {
        self.remove_processor_by_id(&node.id)
    }

    pub fn remove_processor_by_id(&mut self, processor_id: &ProcessorId) -> Result<()> {
        // Validate processor exists in graph
        let processor_exists = {
            let property_graph = self.graph.read();
            property_graph.has_processor(processor_id)
        };

        if !processor_exists {
            return Err(StreamError::ProcessorNotFound(processor_id.clone()));
        }

        // Queue operation for commit - actual shutdown and graph removal happens during commit
        self.pending_operations
            .push(PendingOperation::RemoveProcessor(processor_id.clone()));

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

        // Update config in graph
        {
            let property_graph = self.graph.read();
            let mut graph = property_graph.graph().write();
            graph.update_processor_config(processor_id, config_json)?;
        }

        // Queue operation for commit
        self.pending_operations
            .push(PendingOperation::UpdateProcessorConfig(
                processor_id.clone(),
            ));

        // Publish event
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
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
        use crate::core::context::GpuContext;
        use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};

        use crate::core::pubsub::topics;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        // macOS standalone app setup: detect if we're running as a standalone app
        // (not embedded in another app with its own NSApplication event loop)
        #[cfg(target_os = "macos")]
        {
            use objc2::MainThreadMarker;
            use objc2_app_kit::NSApplication;

            let is_standalone = if let Some(mtm) = MainThreadMarker::new() {
                let app = NSApplication::sharedApplication(mtm);
                !app.isRunning()
            } else {
                // Not on main thread - can't check NSApplication state
                false
            };

            if is_standalone {
                crate::apple::runtime_ext::setup_macos_app();
                crate::apple::runtime_ext::install_macos_shutdown_handler();
            }
        }

        // Initialize runtime context if not already set
        if self.runtime_context.is_none() {
            let gpu = GpuContext::init_for_platform_sync()?;
            self.runtime_context = Some(Arc::new(RuntimeContext::new(gpu)));
        }

        // Set graph state to Running
        self.graph.write().set_state(GraphState::Running);

        // Mark runtime as started so commit() will actually compile
        self.started = true;

        // Compile any pending changes
        self.commit()?;

        // Start all processors that were compiled but not yet started
        {
            let mut property_graph = self.graph.write();
            self.compiler.start_all_processors(&mut property_graph)?;
        }

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
        );

        Ok(())
    }

    /// Stop the runtime.
    pub fn stop(&mut self) -> Result<()> {
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping),
        );

        // Mark runtime as stopped
        self.started = false;

        // Shutdown all processors
        {
            let mut property_graph = self.graph.write();
            shutdown_all_processors(&mut property_graph)?;
        }

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
        );

        Ok(())
    }

    /// Pause the runtime.
    pub fn pause(&mut self) -> Result<()> {
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePausing),
        );

        // Set graph state to Paused
        self.graph.write().set_state(GraphState::Paused);

        // TODO: Signal processors to pause (they should check state periodically)

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePaused),
        );

        Ok(())
    }

    /// Resume the runtime.
    pub fn resume(&mut self) -> Result<()> {
        use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResuming),
        );

        // Set graph state to Running
        self.graph.write().set_state(GraphState::Running);

        // TODO: Signal processors to resume

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
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
