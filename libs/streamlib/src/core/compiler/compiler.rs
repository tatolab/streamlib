//! Core compiler struct that orchestrates graph compilation.

use std::sync::Arc;

use crate::core::compiler::delta::GraphDelta;
use crate::core::compiler::phase::{CompilePhase, CompileResult};
use crate::core::context::RuntimeContext;
use crate::core::delegates::{FactoryDelegate, ProcessorDelegate, SchedulerDelegate};
use crate::core::error::{Result, StreamError};
use crate::core::graph::PropertyGraph;
use crate::core::links::LinkInstanceManager;
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::runtime::delegates::{DefaultProcessorDelegate, DefaultScheduler};

/// Compiles graph changes into running processor state.
pub struct Compiler {
    factory: Arc<dyn FactoryDelegate>,
    processor_delegate: Arc<dyn ProcessorDelegate>,
    scheduler: Arc<dyn SchedulerDelegate>,
}

impl Compiler {
    /// Create a new compiler with the given factory.
    pub fn new<F>(factory: F) -> Self
    where
        F: FactoryDelegate + 'static,
    {
        Self {
            factory: Arc::new(factory),
            processor_delegate: Arc::new(DefaultProcessorDelegate),
            scheduler: Arc::new(DefaultScheduler),
        }
    }

    /// Create a new compiler with full delegate configuration.
    pub fn with_delegates<F, P, S>(factory: F, processor_delegate: P, scheduler: S) -> Self
    where
        F: FactoryDelegate + 'static,
        P: ProcessorDelegate + 'static,
        S: SchedulerDelegate + 'static,
    {
        Self {
            factory: Arc::new(factory),
            processor_delegate: Arc::new(processor_delegate),
            scheduler: Arc::new(scheduler),
        }
    }

    /// Create from pre-wrapped Arc delegates.
    pub fn from_arcs(
        factory: Arc<dyn FactoryDelegate>,
        processor_delegate: Arc<dyn ProcessorDelegate>,
        scheduler: Arc<dyn SchedulerDelegate>,
    ) -> Self {
        Self {
            factory,
            processor_delegate,
            scheduler,
        }
    }

    /// Get a reference to the factory delegate.
    pub fn factory(&self) -> &Arc<dyn FactoryDelegate> {
        &self.factory
    }

    /// Get a reference to the processor delegate.
    pub fn processor_delegate(&self) -> &Arc<dyn ProcessorDelegate> {
        &self.processor_delegate
    }

    /// Get a reference to the scheduler delegate.
    pub fn scheduler(&self) -> &Arc<dyn SchedulerDelegate> {
        &self.scheduler
    }

    /// Compile graph changes.
    ///
    /// Executes the 4-phase pipeline and handles additions, removals, and updates.
    /// Returns a [`CompileResult`] with statistics about what changed.
    pub fn compile(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
    ) -> Result<CompileResult> {
        self.compile_with_options(
            property_graph,
            runtime_context,
            link_instance_manager,
            delta,
            true,
        )
    }

    /// Compile without starting processors (Phase 4 skipped).
    /// Use this during auto-commit; call `start_pending_processors` later.
    pub fn compile_without_start(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
    ) -> Result<CompileResult> {
        self.compile_with_options(
            property_graph,
            runtime_context,
            link_instance_manager,
            delta,
            false,
        )
    }

    /// Compile with options.
    fn compile_with_options(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
        run_start_phase: bool,
    ) -> Result<CompileResult> {
        let mut result = CompileResult::default();

        // Early return if nothing to do
        if delta.is_empty() {
            tracing::debug!("No changes to compile");
            return Ok(result);
        }

        tracing::info!(
            "Compiling: +{} -{} processors, +{} -{} links, {} config updates",
            delta.processors_to_add.len(),
            delta.processors_to_remove.len(),
            delta.links_to_add.len(),
            delta.links_to_remove.len(),
            delta.processors_to_update.len(),
        );

        // Publish compile start event
        self.publish_event(RuntimeEvent::GraphWillCompile);

        // Execute compilation with error handling
        let compile_result = self.execute_phases(
            property_graph,
            runtime_context,
            link_instance_manager,
            delta,
            &mut result,
            run_start_phase,
        );

        match compile_result {
            Ok(()) => {
                // Mark the graph as compiled
                property_graph.mark_compiled();
                self.publish_event(RuntimeEvent::GraphDidCompile);
                tracing::info!("Compile complete: {}", result);
                Ok(result)
            }
            Err(e) => {
                self.publish_event(RuntimeEvent::GraphCompileFailed {
                    error: e.to_string(),
                });
                tracing::error!("Compile failed: {}", e);
                Err(e)
            }
        }
    }

    /// Execute all compilation phases.
    fn execute_phases(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
        result: &mut CompileResult,
        run_start_phase: bool,
    ) -> Result<()> {
        // First: Handle removals (before adding new processors)
        // This ensures clean shutdown of removed components
        self.handle_removals(property_graph, link_instance_manager, delta, result)?;

        // Phase 1: CREATE - Instantiate processor instances
        self.run_phase(CompilePhase::Create, || {
            self.phase_create(property_graph, delta, result)
        })?;

        // Phase 2: WIRE - Create ring buffers and connect ports
        self.run_phase(CompilePhase::Wire, || {
            self.phase_wire(property_graph, link_instance_manager, delta, result)
        })?;

        // Phase 3: SETUP - Initialize processors (GPU, devices)
        self.run_phase(CompilePhase::Setup, || {
            self.phase_setup(property_graph, runtime_context, delta)
        })?;

        // Phase 4: START - Spawn processor threads (skipped during auto-commit)
        if run_start_phase {
            self.run_phase(CompilePhase::Start, || {
                self.phase_start(property_graph, delta)
            })?;
        } else {
            tracing::debug!("[Phase 4: START] Deferred until runtime.start()");
        }

        // Handle config updates (can happen on running processors)
        self.handle_config_updates(property_graph, delta, result)?;

        Ok(())
    }

    /// Run a phase with logging.
    fn run_phase<F>(&self, phase: CompilePhase, f: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        tracing::debug!("[{}] Starting", phase);
        let result = f();
        match &result {
            Ok(()) => tracing::debug!("[{}] Completed", phase),
            Err(e) => tracing::error!("[{}] Failed: {}", phase, e),
        }
        result
    }

    /// Handle processor and link removals.
    fn handle_removals(
        &self,
        property_graph: &mut PropertyGraph,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        // Unwire links first (before removing processors)
        for link_id in &delta.links_to_remove {
            // Get link info for event before removal
            if let Some(link) = property_graph.get_link(link_id) {
                let from_port = link.from_port().to_string();
                let to_port = link.to_port().to_string();

                self.publish_event(RuntimeEvent::GraphWillRemoveLink {
                    link_id: link_id.to_string(),
                    from_port: from_port.clone(),
                    to_port: to_port.clone(),
                });

                tracing::info!("[UNWIRE] {}", link_id);
                if let Err(e) = super::wiring::unwire_link(property_graph, link_id) {
                    tracing::warn!("Failed to unwire link {}: {}", link_id, e);
                }
                link_instance_manager.disconnect(link_id.clone());

                self.publish_event(RuntimeEvent::GraphDidRemoveLink {
                    link_id: link_id.to_string(),
                    from_port,
                    to_port,
                });

                result.links_unwired += 1;
            }
        }

        // Shutdown and remove processors
        for proc_id in &delta.processors_to_remove {
            self.publish_event(RuntimeEvent::GraphWillRemoveProcessor {
                processor_id: proc_id.clone(),
            });

            tracing::info!("[REMOVE] {}", proc_id);
            self.processor_delegate.will_stop(proc_id)?;

            if let Err(e) = super::phases::shutdown_processor(property_graph, proc_id) {
                tracing::warn!("Failed to shutdown processor {}: {}", proc_id, e);
            }

            self.processor_delegate.did_stop(proc_id)?;

            self.publish_event(RuntimeEvent::GraphDidRemoveProcessor {
                processor_id: proc_id.clone(),
            });

            result.processors_removed += 1;
        }

        Ok(())
    }

    /// Phase 1: CREATE - Instantiate processor instances.
    fn phase_create(
        &self,
        property_graph: &mut PropertyGraph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            let node = property_graph.get_processor(proc_id).ok_or_else(|| {
                StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
            })?;

            let processor_type = node.processor_type.clone();

            self.publish_event(RuntimeEvent::GraphWillAddProcessor {
                processor_id: proc_id.clone(),
                processor_type: processor_type.clone(),
            });

            tracing::info!("[{}] Creating {}", CompilePhase::Create, proc_id);

            super::phases::create_processor(
                &self.factory,
                &self.processor_delegate,
                property_graph,
                proc_id,
            )?;

            self.publish_event(RuntimeEvent::GraphDidAddProcessor {
                processor_id: proc_id.clone(),
                processor_type,
            });

            result.processors_created += 1;
        }
        Ok(())
    }

    /// Phase 2: WIRE - Create ring buffers and connect ports.
    fn phase_wire(
        &self,
        property_graph: &mut PropertyGraph,
        link_instance_manager: &mut LinkInstanceManager,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        for link_id in &delta.links_to_add {
            // Get link info for event
            let link = property_graph.get_link(link_id).ok_or_else(|| {
                StreamError::LinkNotFound(format!("Link '{}' not found", link_id))
            })?;

            let from_port = link.from_port().to_string();
            let to_port = link.to_port().to_string();

            // Parse port addresses for event
            let (from_processor, from_port_name) =
                super::wiring::parse_port_address(&from_port).unwrap_or_default();
            let (to_processor, to_port_name) =
                super::wiring::parse_port_address(&to_port).unwrap_or_default();

            self.publish_event(RuntimeEvent::GraphWillCreateLink {
                from_processor,
                from_port: from_port_name,
                to_processor,
                to_port: to_port_name,
            });

            tracing::info!("[{}] Wiring {}", CompilePhase::Wire, link_id);

            super::wiring::wire_link(property_graph, link_instance_manager, link_id)?;

            self.publish_event(RuntimeEvent::GraphDidCreateLink {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            });

            result.links_wired += 1;
        }
        Ok(())
    }

    /// Phase 3: SETUP - Initialize processors (GPU, devices).
    fn phase_setup(
        &self,
        property_graph: &mut PropertyGraph,
        runtime_context: &Arc<RuntimeContext>,
        delta: &GraphDelta,
    ) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            tracing::info!("[{}] Setting up {}", CompilePhase::Setup, proc_id);
            super::phases::setup_processor(property_graph, runtime_context, proc_id)?;
        }
        Ok(())
    }

    /// Phase 4: START - Spawn processor threads.
    fn phase_start(&self, property_graph: &mut PropertyGraph, delta: &GraphDelta) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            tracing::info!("[{}] Starting {}", CompilePhase::Start, proc_id);
            super::phases::start_processor(
                &self.processor_delegate,
                &self.scheduler,
                property_graph,
                proc_id,
            )?;
        }
        Ok(())
    }

    /// Handle config updates for running processors.
    fn handle_config_updates(
        &self,
        property_graph: &mut PropertyGraph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        use crate::core::graph::ProcessorInstance;

        for config_change in &delta.processors_to_update {
            let proc_id = &config_change.id;

            tracing::info!(
                "[CONFIG] Updating {} (checksum {} -> {})",
                proc_id,
                config_change.old_config_checksum,
                config_change.new_config_checksum
            );

            // Get the new config from the graph node
            let node = property_graph.get_processor(proc_id).ok_or_else(|| {
                StreamError::ProcessorNotFound(format!(
                    "Processor '{}' not found for config update",
                    proc_id
                ))
            })?;

            // Config may be None if using defaults
            let config_json = match &node.config {
                Some(config) => config.clone(),
                None => {
                    tracing::debug!("[CONFIG] {} has no config to update", proc_id);
                    continue;
                }
            };

            // Apply config to running processor instance
            if let Some(instance) = property_graph.get::<ProcessorInstance>(proc_id) {
                let mut guard = instance.0.lock();
                guard.apply_config_json(&config_json)?;
                drop(guard);

                // Delegate callback
                self.processor_delegate
                    .did_update_config(proc_id, &config_json)?;

                // Publish event
                self.publish_event(RuntimeEvent::ProcessorConfigDidChange {
                    processor_id: proc_id.clone(),
                });

                result.configs_updated += 1;
            }
        }
        Ok(())
    }

    /// Publish an event to the global PubSub.
    fn publish_event(&self, event: RuntimeEvent) {
        let event = Event::RuntimeGlobal(event);
        PUBSUB.publish(&topics::RUNTIME_GLOBAL, &event);
    }

    /// Start all processors that have been compiled but not yet started.
    pub fn start_all_processors(&self, property_graph: &mut PropertyGraph) -> Result<()> {
        use crate::core::graph::{ProcessorInstance, ThreadHandle};

        // Find all processors with ProcessorInstance but no ThreadHandle (compiled but not started)
        let processors_to_start: Vec<String> = property_graph
            .processor_ids()
            .filter(|proc_id| {
                property_graph.has::<ProcessorInstance>(proc_id)
                    && !property_graph.has::<ThreadHandle>(proc_id)
            })
            .cloned()
            .collect();

        if processors_to_start.is_empty() {
            tracing::debug!("No pending processors to start");
            return Ok(());
        }

        tracing::info!("Starting {} pending processors", processors_to_start.len());

        for proc_id in &processors_to_start {
            tracing::info!("[Phase 4: START] Starting {}", proc_id);
            super::phases::start_processor(
                &self.processor_delegate,
                &self.scheduler,
                property_graph,
                proc_id,
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::delegates::DefaultFactory;

    #[test]
    fn test_compiler_creation() {
        // New ergonomic API - no Arc::new() needed
        let compiler = Compiler::new(DefaultFactory::new());
        assert!(!compiler.factory().can_create("unknown"));
    }

    #[test]
    fn test_compiler_with_delegates() {
        // New ergonomic API - no Arc::new() needed
        let compiler = Compiler::with_delegates(
            DefaultFactory::new(),
            DefaultProcessorDelegate,
            DefaultScheduler,
        );
        assert!(!compiler.factory().can_create("unknown"));
    }

    #[test]
    fn test_compiler_from_arcs() {
        // Legacy API for pre-wrapped Arcs
        let factory = Arc::new(DefaultFactory::new());
        let processor_delegate = Arc::new(DefaultProcessorDelegate);
        let scheduler = Arc::new(DefaultScheduler);

        let compiler = Compiler::from_arcs(factory, processor_delegate, scheduler);
        assert!(!compiler.factory().can_create("unknown"));
    }
}
