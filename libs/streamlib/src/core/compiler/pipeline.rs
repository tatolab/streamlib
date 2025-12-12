// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Core compiler struct that orchestrates graph compilation.

use std::sync::Arc;

use crate::core::compiler::delta::GraphDelta;
use crate::core::compiler::phase::{CompilePhase, CompileResult};
use crate::core::context::RuntimeContext;
use crate::core::delegates::{FactoryDelegate, LinkDelegate, ProcessorDelegate, SchedulerDelegate};
use crate::core::error::{Result, StreamError};
use crate::core::graph::{Graph, GraphNode, ProcessorUniqueId};
use crate::core::links::{DefaultLinkFactory, LinkFactoryDelegate};
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::runtime::delegates::{
    DefaultLinkDelegate, DefaultProcessorDelegate, DefaultScheduler,
};

/// Compiles graph changes into running processor state.
pub struct Compiler {
    factory: Arc<dyn FactoryDelegate>,
    processor_delegate: Arc<dyn ProcessorDelegate>,
    link_delegate: Arc<dyn LinkDelegate>,
    scheduler: Arc<dyn SchedulerDelegate>,
    link_factory: Arc<dyn LinkFactoryDelegate>,
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
            link_delegate: Arc::new(DefaultLinkDelegate),
            scheduler: Arc::new(DefaultScheduler),
            link_factory: Arc::new(DefaultLinkFactory),
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
            link_delegate: Arc::new(DefaultLinkDelegate),
            scheduler: Arc::new(scheduler),
            link_factory: Arc::new(DefaultLinkFactory),
        }
    }

    /// Create a new compiler with all delegates including link factory and link delegate.
    pub fn with_all_delegates<F, P, L, S, LF>(
        factory: F,
        processor_delegate: P,
        link_delegate: L,
        scheduler: S,
        link_factory: LF,
    ) -> Self
    where
        F: FactoryDelegate + 'static,
        P: ProcessorDelegate + 'static,
        L: LinkDelegate + 'static,
        S: SchedulerDelegate + 'static,
        LF: LinkFactoryDelegate + 'static,
    {
        Self {
            factory: Arc::new(factory),
            processor_delegate: Arc::new(processor_delegate),
            link_delegate: Arc::new(link_delegate),
            scheduler: Arc::new(scheduler),
            link_factory: Arc::new(link_factory),
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
            link_delegate: Arc::new(DefaultLinkDelegate),
            scheduler,
            link_factory: Arc::new(DefaultLinkFactory),
        }
    }

    /// Create from pre-wrapped Arc delegates including link factory.
    pub fn from_arcs_with_link_factory(
        factory: Arc<dyn FactoryDelegate>,
        processor_delegate: Arc<dyn ProcessorDelegate>,
        scheduler: Arc<dyn SchedulerDelegate>,
        link_factory: Arc<dyn LinkFactoryDelegate>,
    ) -> Self {
        Self {
            factory,
            processor_delegate,
            link_delegate: Arc::new(DefaultLinkDelegate),
            scheduler,
            link_factory,
        }
    }

    /// Create from pre-wrapped Arc delegates including all delegates.
    pub fn from_all_arcs(
        factory: Arc<dyn FactoryDelegate>,
        processor_delegate: Arc<dyn ProcessorDelegate>,
        link_delegate: Arc<dyn LinkDelegate>,
        scheduler: Arc<dyn SchedulerDelegate>,
        link_factory: Arc<dyn LinkFactoryDelegate>,
    ) -> Self {
        Self {
            factory,
            processor_delegate,
            link_delegate,
            scheduler,
            link_factory,
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

    /// Get a reference to the link delegate.
    pub fn link_delegate(&self) -> &Arc<dyn LinkDelegate> {
        &self.link_delegate
    }

    /// Get a reference to the scheduler delegate.
    pub fn scheduler(&self) -> &Arc<dyn SchedulerDelegate> {
        &self.scheduler
    }

    /// Get a reference to the link factory delegate.
    pub fn link_factory(&self) -> &Arc<dyn LinkFactoryDelegate> {
        &self.link_factory
    }

    /// Compile graph changes.
    ///
    /// Executes the 4-phase pipeline and handles additions, removals, and updates.
    /// Returns a [`CompileResult`] with statistics about what changed.
    pub fn compile(
        &self,
        property_graph: &mut Graph,
        runtime_context: &Arc<RuntimeContext>,
        delta: &GraphDelta,
    ) -> Result<CompileResult> {
        self.compile_with_options(property_graph, runtime_context, delta, true)
    }

    /// Compile without starting processors (Phase 4 skipped).
    /// Use this during auto-commit; call `start_pending_processors` later.
    pub fn compile_without_start(
        &self,
        property_graph: &mut Graph,
        runtime_context: &Arc<RuntimeContext>,
        delta: &GraphDelta,
    ) -> Result<CompileResult> {
        self.compile_with_options(property_graph, runtime_context, delta, false)
    }

    /// Compile with options.
    fn compile_with_options(
        &self,
        property_graph: &mut Graph,
        runtime_context: &Arc<RuntimeContext>,
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
        self.publish_event(RuntimeEvent::CompilerWillCompile);

        // Execute compilation with error handling
        let compile_result = self.execute_phases(
            property_graph,
            runtime_context,
            delta,
            &mut result,
            run_start_phase,
        );

        match compile_result {
            Ok(()) => {
                // Mark the graph as compiled
                property_graph.mark_compiled();
                self.publish_event(RuntimeEvent::CompilerDidCompile);
                tracing::info!("Compile complete: {}", result);
                Ok(result)
            }
            Err(e) => {
                self.publish_event(RuntimeEvent::CompilerDidFail {
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
        property_graph: &mut Graph,
        runtime_context: &Arc<RuntimeContext>,
        delta: &GraphDelta,
        result: &mut CompileResult,
        run_start_phase: bool,
    ) -> Result<()> {
        // First: Handle removals (before adding new processors)
        // This ensures clean shutdown of removed components
        self.handle_removals(property_graph, delta, result)?;

        // Phase 1: CREATE - Instantiate processor instances
        self.run_phase(CompilePhase::Create, || {
            self.phase_create(property_graph, delta, result)
        })?;

        // Phase 2: WIRE - Create ring buffers and connect ports
        self.run_phase(CompilePhase::Wire, || {
            self.phase_wire(property_graph, delta, result)
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
        graph: &mut Graph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        // Unwire links first (before removing processors)
        for link_id in &delta.links_to_remove {
            // Get link info for event before removal
            if let Some(link) = graph
                .traversal()
                .e(())
                .filter(|link| link.id == *link_id)
                .first()
            {
                let from_port = link.from_port().to_string();
                let to_port = link.to_port().to_string();

                self.publish_event(RuntimeEvent::CompilerWillUnwireLink {
                    link_id: link_id.to_string(),
                    from_port: from_port.clone(),
                    to_port: to_port.clone(),
                });

                // Call link delegate will_unwire hook
                self.link_delegate.will_unwire(link_id)?;

                tracing::info!("[UNWIRE] {}", link_id);
                if let Err(e) = super::wiring::unwire_link(graph, link_id) {
                    tracing::warn!("Failed to unwire link {}: {}", link_id, e);
                }

                // Call link delegate did_unwire hook
                self.link_delegate.did_unwire(link_id)?;

                self.publish_event(RuntimeEvent::CompilerDidUnwireLink {
                    link_id: link_id.to_string(),
                    from_port,
                    to_port,
                });

                result.links_unwired += 1;
            }
        }

        // Shutdown and remove processors
        for proc_id in &delta.processors_to_remove {
            self.publish_event(RuntimeEvent::CompilerWillDestroyProcessor {
                processor_id: proc_id.clone(),
            });

            tracing::info!("[REMOVE] {}", proc_id);
            self.processor_delegate.will_stop(proc_id)?;

            if let Err(e) = super::phases::shutdown_processor(graph, proc_id) {
                tracing::warn!("Failed to shutdown processor {}: {}", proc_id, e);
            }

            self.processor_delegate.did_stop(proc_id)?;

            self.publish_event(RuntimeEvent::CompilerDidDestroyProcessor {
                processor_id: proc_id.clone(),
            });

            result.processors_removed += 1;
        }

        Ok(())
    }

    /// Phase 1: CREATE - Instantiate processor instances.
    fn phase_create(
        &self,
        graph: &mut Graph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            let node = graph.traversal().v(proc_id).first().ok_or_else(|| {
                StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
            })?;

            let processor_type = node.processor_type.clone();

            self.publish_event(RuntimeEvent::CompilerWillCreateProcessor {
                processor_id: proc_id.clone(),
                processor_type: processor_type.clone(),
            });

            tracing::info!("[{}] Creating {}", CompilePhase::Create, proc_id);

            super::phases::create_processor(
                &self.factory,
                &self.processor_delegate,
                graph,
                proc_id,
            )?;

            self.publish_event(RuntimeEvent::CompilerDidCreateProcessor {
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
        graph: &mut Graph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        for link_id in &delta.links_to_add {
            // Get link info for event - extract what we need then drop the borrow
            let (from_port, to_port) = {
                let link = graph.traversal().e(link_id).first().ok_or_else(|| {
                    StreamError::LinkNotFound(format!("Link '{}' not found", link_id))
                })?;
                (link.from_port().to_string(), link.to_port().to_string())
            };

            self.publish_event(RuntimeEvent::CompilerWillWireLink {
                link_id: link_id.to_string(),
                from_port: from_port.clone(),
                to_port: to_port.clone(),
            });

            // Call link delegate will_wire hook
            {
                let link = graph.traversal().e(link_id).first().ok_or_else(|| {
                    StreamError::LinkNotFound(format!("Link '{}' not found", link_id))
                })?;
                self.link_delegate.will_wire(link)?;
            }

            tracing::info!("[{}] Wiring {}", CompilePhase::Wire, link_id);

            super::wiring::wire_link(graph, self.link_factory.as_ref(), link_id)?;

            // Call link delegate did_wire hook
            {
                let link = graph.traversal().e(link_id).first().ok_or_else(|| {
                    StreamError::LinkNotFound(format!("Link '{}' not found", link_id))
                })?;
                self.link_delegate.did_wire(link)?;
            }

            self.publish_event(RuntimeEvent::CompilerDidWireLink {
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
        property_graph: &mut Graph,
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
    fn phase_start(&self, property_graph: &mut Graph, delta: &GraphDelta) -> Result<()> {
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

    /// Handle config updates on existing processors.
    fn handle_config_updates(
        &self,
        property_graph: &mut Graph,
        delta: &GraphDelta,
        result: &mut CompileResult,
    ) -> Result<()> {
        use crate::core::graph::ProcessorInstanceComponent;

        for update in &delta.processors_to_update {
            let proc_id = &update.id;

            // Get config from the ProcessorNode in the graph - clone it to avoid borrow issues
            let config_json = match property_graph.traversal().v(proc_id).first() {
                Some(node) => match &node.config {
                    Some(config) => config.clone(),
                    None => {
                        tracing::debug!("[CONFIG] {} has no config to update", proc_id);
                        continue;
                    }
                },
                None => {
                    tracing::warn!("[CONFIG] Processor {} not found in graph", proc_id);
                    continue;
                }
            };

            // Get the ProcessorInstance and apply config
            let processor_arc = property_graph
                .traversal()
                .v(proc_id)
                .first()
                .and_then(|node| {
                    node.get::<ProcessorInstanceComponent>()
                        .map(|i| i.0.clone())
                })
                .ok_or_else(|| {
                    StreamError::ProcessorNotFound(format!(
                        "Processor '{}' not found for config update",
                        proc_id
                    ))
                })?;

            // Apply config update
            {
                let mut guard = processor_arc.lock();
                guard.apply_config_json(&config_json)?;
            }

            // Notify delegate
            self.processor_delegate
                .did_update_config(proc_id, &config_json)?;

            tracing::info!("[CONFIG] Updated config for {}", proc_id);
            result.configs_updated += 1;
        }

        Ok(())
    }

    /// Publish an event to the global PubSub.
    fn publish_event(&self, event: RuntimeEvent) {
        let event = Event::RuntimeGlobal(event);
        PUBSUB.publish(topics::RUNTIME_GLOBAL, &event);
    }

    /// Start all processors that have been compiled but not yet started.
    pub fn start_all_processors(&self, property_graph: &mut Graph) -> Result<()> {
        use crate::core::graph::{GraphNode, ProcessorInstanceComponent, ThreadHandleComponent};

        // Find all processors with ProcessorInstance but no ThreadHandle (compiled but not started)
        let processors_to_start: Vec<ProcessorUniqueId> = property_graph
            .traversal()
            .v(())
            .filter(|node| node.has::<ProcessorInstanceComponent>())
            .filter(|node| !node.has::<ThreadHandleComponent>())
            .ids();

        for proc_id in processors_to_start {
            tracing::info!("[{}] Starting {}", CompilePhase::Start, proc_id);
            super::phases::start_processor(
                &self.processor_delegate,
                &self.scheduler,
                property_graph,
                &proc_id,
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
        let factory = DefaultFactory::new();
        let compiler = Compiler::new(factory);

        assert!(Arc::strong_count(compiler.factory()) >= 1);
        assert!(Arc::strong_count(compiler.processor_delegate()) >= 1);
        assert!(Arc::strong_count(compiler.scheduler()) >= 1);
        assert!(Arc::strong_count(compiler.link_factory()) >= 1);
    }

    #[test]
    fn test_compiler_with_delegates() {
        let factory = DefaultFactory::new();
        let processor_delegate = DefaultProcessorDelegate;
        let scheduler = DefaultScheduler;

        let compiler = Compiler::with_delegates(factory, processor_delegate, scheduler);

        assert!(Arc::strong_count(compiler.factory()) >= 1);
    }

    #[test]
    fn test_compiler_from_arcs() {
        let factory: Arc<dyn FactoryDelegate> = Arc::new(DefaultFactory::new());
        let processor_delegate: Arc<dyn ProcessorDelegate> = Arc::new(DefaultProcessorDelegate);
        let scheduler: Arc<dyn SchedulerDelegate> = Arc::new(DefaultScheduler);

        let compiler = Compiler::from_arcs(
            Arc::clone(&factory),
            Arc::clone(&processor_delegate),
            Arc::clone(&scheduler),
        );

        assert!(Arc::strong_count(compiler.factory()) >= 2);
    }
}
