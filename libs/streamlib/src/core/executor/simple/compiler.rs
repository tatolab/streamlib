use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::context::{GpuContext, RuntimeContext};
use crate::core::error::{Result, StreamError};
use crate::core::executor::delta::{compute_delta_with_config, GraphDelta};
use crate::core::executor::execution_graph::{CompilationMetadata, ExecutionGraph};
use crate::core::executor::{ExecutorState, GraphCompiler};
use crate::core::graph::ProcessorId;
use crate::core::link_channel::LinkId;

use super::SimpleExecutor;

impl GraphCompiler for SimpleExecutor {
    fn compile(&mut self) -> Result<()> {
        if self.state != ExecutorState::Running {
            tracing::debug!("compile() called but executor not running, skipping");
            return Ok(());
        }

        tracing::info!("Compiling graph...");

        self.init_execution_graph_if_needed()?;

        let delta = self.compute_graph_delta()?;
        tracing::info!(
            "Compiling: {} processors, {} links",
            delta.processors_to_add.len(),
            delta.links_to_add.len()
        );

        self.compile_phase_create(&delta)?;
        self.compile_phase_wire(&delta)?;
        self.compile_phase_setup(&delta)?;
        self.compile_phase_start(&delta)?;

        self.dirty = false;
        tracing::info!("Compile complete");
        Ok(())
    }

    fn create_processor(&mut self, proc_id: &ProcessorId) -> Result<()> {
        super::processors::create_processor(self, proc_id)
    }

    fn wire_link(&mut self, link_id: &LinkId) -> Result<()> {
        super::wiring::wire_link(self, link_id)
    }

    fn setup_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        super::processors::setup_processor(self, processor_id)
    }

    fn start_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        super::processors::start_processor(self, processor_id)
    }

    fn shutdown_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        super::processors::shutdown_processor(self, processor_id)
    }
}

// ============================================================================
// Compilation phases
// ============================================================================

impl SimpleExecutor {
    pub(super) fn init_execution_graph_if_needed(&mut self) -> Result<()> {
        if self.execution_graph.is_some() {
            return Ok(());
        }

        let graph = self
            .graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))?;

        let source_checksum = {
            let graph_guard = graph.read();
            graph_guard.validate()?;
            graph_guard.checksum()
        };

        let gpu_context = GpuContext::init_for_platform_sync()?;
        let runtime_context = RuntimeContext::new(gpu_context);
        self.runtime_context = Some(Arc::new(runtime_context));

        let metadata = CompilationMetadata::new(source_checksum);
        let graph_clone = Arc::clone(graph);
        self.execution_graph = Some(ExecutionGraph::new(graph_clone, metadata));

        tracing::info!(
            "Execution graph initialized (checksum: {:?})",
            source_checksum
        );

        Ok(())
    }

    fn compile_phase_create(&mut self, delta: &GraphDelta) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            tracing::info!("[Phase 1: CREATE] {}", proc_id);
            GraphCompiler::create_processor(self, proc_id)?;
        }
        Ok(())
    }

    fn compile_phase_wire(&mut self, delta: &GraphDelta) -> Result<()> {
        for link_id in &delta.links_to_add {
            tracing::info!("[Phase 2: WIRE] {}", link_id);
            GraphCompiler::wire_link(self, link_id)?;
        }
        Ok(())
    }

    fn compile_phase_setup(&mut self, delta: &GraphDelta) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            tracing::info!("[Phase 3: SETUP] {}", proc_id);
            GraphCompiler::setup_processor(self, proc_id)?;
        }
        Ok(())
    }

    fn compile_phase_start(&mut self, delta: &GraphDelta) -> Result<()> {
        for proc_id in &delta.processors_to_add {
            tracing::info!("[Phase 4: START] {}", proc_id);
            GraphCompiler::start_processor(self, proc_id)?;
        }
        Ok(())
    }
}

// ============================================================================
// Delta computation
// ============================================================================

impl SimpleExecutor {
    pub(super) fn compute_graph_delta(&self) -> Result<GraphDelta> {
        let graph = self.graph_ref()?;
        let exec_graph = self.exec_graph_ref()?;
        let graph_guard = graph.read();

        // Desired state from graph
        let graph_processor_ids: HashSet<ProcessorId> =
            graph_guard.nodes().iter().map(|n| n.id.clone()).collect();
        let graph_link_ids: HashSet<LinkId> =
            graph_guard.links().iter().map(|l| l.id.clone()).collect();
        let graph_config_checksums: HashMap<ProcessorId, u64> = graph_guard
            .nodes()
            .iter()
            .map(|n| (n.id.clone(), n.config_checksum))
            .collect();

        // Current state from execution graph
        let running_processor_ids: HashSet<ProcessorId> =
            exec_graph.processor_ids().cloned().collect();
        let wired_link_ids: HashSet<LinkId> = exec_graph
            .iter_link_runtime()
            .map(|(id, _)| id.clone())
            .collect();
        let running_config_checksums: HashMap<ProcessorId, u64> = exec_graph
            .iter_processor_runtime()
            .map(|(id, proc)| (id.clone(), proc.node.config_checksum))
            .collect();

        Ok(compute_delta_with_config(
            &graph_processor_ids,
            &graph_link_ids,
            &running_processor_ids,
            &wired_link_ids,
            &graph_config_checksums,
            &running_config_checksums,
        ))
    }

    #[allow(dead_code)]
    pub(super) fn apply_delta(&mut self, delta: GraphDelta) -> Result<()> {
        // Step 1: Unwire removed links
        for link_id in &delta.links_to_remove {
            tracing::info!("Removing link: {}", link_id);
            if let Err(e) = super::wiring::unwire_link(self, link_id) {
                tracing::warn!("Error unwiring link {}: {}", link_id, e);
            }
        }

        // Step 2: Shutdown removed processors
        for proc_id in &delta.processors_to_remove {
            tracing::info!("Removing processor: {}", proc_id);
            if let Err(e) = GraphCompiler::shutdown_processor(self, proc_id) {
                tracing::warn!("Error shutting down processor {}: {}", proc_id, e);
            }
            if let Some(exec_graph) = &mut self.execution_graph {
                exec_graph.remove_processor_runtime(proc_id);
            }
        }

        // Step 3: Create new processors
        for proc_id in &delta.processors_to_add {
            tracing::info!("Adding processor: {}", proc_id);
            GraphCompiler::create_processor(self, proc_id)?;
        }

        // Step 4: Wire new links
        for link_id in &delta.links_to_add {
            tracing::info!("Adding link: {}", link_id);
            GraphCompiler::wire_link(self, link_id)?;
        }

        // Step 5: Apply config changes
        for config_change in &delta.processors_to_update {
            tracing::info!(
                "Updating processor config: {} (checksum {} -> {})",
                config_change.id,
                config_change.old_config_checksum,
                config_change.new_config_checksum
            );
            if let Err(e) = self.apply_config_change(&config_change.id) {
                tracing::warn!(
                    "Error applying config change to processor {}: {}",
                    config_change.id,
                    e
                );
            }
        }

        self.dirty = false;
        Ok(())
    }

    #[allow(dead_code)]
    pub(super) fn apply_config_change(&mut self, proc_id: &ProcessorId) -> Result<()> {
        // Get the new config from the graph
        let new_config = {
            let graph = self.graph_ref()?;
            let graph_guard = graph.read();
            let node = graph_guard.get_processor(proc_id).ok_or_else(|| {
                StreamError::ProcessorNotFound(format!(
                    "Processor '{}' not found in graph",
                    proc_id
                ))
            })?;
            node.config.clone()
        };

        let exec_graph = self.exec_graph_mut()?;
        let running = exec_graph
            .get_processor_runtime_mut(proc_id)
            .ok_or_else(|| {
                StreamError::ProcessorNotFound(format!(
                    "Processor '{}' not found in execution graph",
                    proc_id
                ))
            })?;

        // Apply config via the DynProcessor trait
        if let Some(processor_arc) = &running.processor {
            if let Some(config) = &new_config {
                let mut proc_guard = processor_arc.lock();
                proc_guard.apply_config_json(config)?;
            }
        }

        // Update the checksum in the running processor's node
        let new_checksum = {
            let graph = self.graph_ref()?;
            let graph_guard = graph.read();
            graph_guard
                .get_processor(proc_id)
                .map(|n| n.config_checksum)
                .unwrap_or(0)
        };

        let exec_graph = self.exec_graph_mut()?;
        if let Some(running) = exec_graph.get_processor_runtime_mut(proc_id) {
            running.node.config = new_config;
            running.node.config_checksum = new_checksum;
        }

        Ok(())
    }
}
