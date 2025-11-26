use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use super::execution_graph::{CompilationMetadata, ExecutionGraph};
use super::running::{RunningProcessor, WiredLink};
use super::{Executor, ExecutorState};
use crate::core::context::{GpuContext, RuntimeContext};
use crate::core::error::{Result, StreamError};
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{Graph, ProcessorId};
use crate::core::link_channel::{
    LinkChannel, LinkId, LinkPortAddress, LinkPortType, LinkWakeupEvent,
};
use crate::core::processors::{DynProcessor, ProcessorState};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode};

/// Type alias for boxed dynamic processor
pub type BoxedProcessor = Box<dyn DynProcessor + Send>;

/// Runtime status snapshot
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// Whether the runtime is currently running
    pub running: bool,
    /// Number of processors registered
    pub processor_count: usize,
    /// Number of active connections
    pub connection_count: usize,
    /// Per-processor state
    pub processor_states: HashMap<ProcessorId, ProcessorState>,
}

/// Simple executor - thread-per-processor with lock-free connections
pub struct SimpleExecutor {
    state: ExecutorState,
    /// Shared graph (DOM) - read-only access to topology
    graph: Option<Arc<RwLock<Graph>>>,
    /// Runtime context (GPU + main thread dispatch)
    runtime_context: Option<Arc<RuntimeContext>>,
    /// Execution graph (VDOM) - runtime state extending the Graph
    /// Created during compile(), contains RunningProcessors and WiredLinks
    execution_graph: Option<ExecutionGraph>,
    /// Processors waiting to be started (from runtime's pending_processors)
    pending_processors: Vec<(ProcessorId, BoxedProcessor)>,
    /// Connection bus (creates ring buffers)
    link_channel: LinkChannel,
    /// Connections to wire on start (from_port, to_port)
    connections_to_wire: Vec<(String, String)>,
    /// Next processor ID counter
    next_processor_id: usize,
    /// Next connection ID counter
    next_connection_id: usize,
    /// Graph has changed since last compile
    dirty: bool,
}

impl Default for SimpleExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleExecutor {
    /// Create a new simple executor (standalone, no shared graph)
    pub fn new() -> Self {
        Self {
            state: ExecutorState::Idle,
            graph: None,
            runtime_context: None,
            execution_graph: None,
            pending_processors: Vec::new(),
            link_channel: LinkChannel::new(),
            connections_to_wire: Vec::new(),
            next_processor_id: 0,
            next_connection_id: 0,
            dirty: false,
        }
    }

    /// Create a new simple executor with a shared graph reference
    ///
    /// The executor reads the graph on compile/start to understand the topology.
    /// The runtime modifies the graph, and the executor sees changes via the shared reference.
    pub fn with_graph(graph: Arc<RwLock<Graph>>) -> Self {
        Self {
            state: ExecutorState::Idle,
            graph: Some(graph),
            runtime_context: None,
            execution_graph: None,
            pending_processors: Vec::new(),
            link_channel: LinkChannel::new(),
            connections_to_wire: Vec::new(),
            next_processor_id: 0,
            next_connection_id: 0,
            dirty: false,
        }
    }

    /// Register a processor with the executor
    ///
    /// Called by runtime.add_processor() to delegate processor ownership to the executor.
    /// The executor owns all processor instances - the runtime only knows about the Graph.
    pub fn register_processor(&mut self, id: ProcessorId, processor: BoxedProcessor) {
        self.pending_processors.push((id, processor));
        self.dirty = true;
    }

    /// Compile the execution plan from the shared graph
    ///
    /// This is the "DOM to VDOM" step - reads the Graph (DOM) and creates
    /// the ExecutionGraph (VDOM) with runtime metadata.
    ///
    /// Called automatically by start() if needed.
    fn compile_from_graph(&mut self) -> Result<()> {
        // Get the graph (either shared or we need one passed in)
        let graph = self
            .graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))?;

        // Read graph to extract connections
        let graph_guard = graph.read();

        // Validate graph (checks for cycles, etc.)
        graph_guard.validate()?;

        // Compute checksum of the source graph for cache invalidation
        let source_checksum = graph_guard.checksum();

        // Extract connections to wire from the graph links
        self.connections_to_wire.clear();
        for edge in graph_guard.petgraph().edge_indices() {
            let link = &graph_guard.petgraph()[edge];
            self.connections_to_wire
                .push((link.from_port(), link.to_port()));
        }

        // Drop the guard before creating context (might need GPU init)
        drop(graph_guard);

        // Create runtime context (GPU + main thread dispatch)
        let gpu_context = GpuContext::init_for_platform_sync()?;
        let runtime_context = RuntimeContext::new(gpu_context);
        self.runtime_context = Some(Arc::new(runtime_context));

        // Create the execution graph (VDOM) with compilation metadata
        // ExecutionGraph wraps the Graph and adds runtime state
        let metadata = CompilationMetadata::new(source_checksum);
        self.execution_graph = Some(ExecutionGraph::new(Arc::clone(graph), metadata));

        self.dirty = false;
        self.state = ExecutorState::Compiled;

        tracing::info!(
            "Graph compiled successfully (checksum: {:?})",
            source_checksum
        );
        Ok(())
    }

    /// Set runtime context
    pub fn set_runtime_context(&mut self, ctx: Arc<RuntimeContext>) {
        self.runtime_context = Some(ctx);
    }

    /// Get runtime context
    pub fn runtime_context(&self) -> Option<&RuntimeContext> {
        self.runtime_context.as_ref().map(|arc| arc.as_ref())
    }

    /// Get runtime status snapshot
    pub fn status(&self) -> RuntimeStatus {
        let (processor_count, connection_count, processor_states) =
            if let Some(exec_graph) = &self.execution_graph {
                let states = exec_graph
                    .iter_processor_runtime()
                    .map(|(id, proc)| (id.clone(), *proc.state.lock()))
                    .collect();
                (
                    exec_graph.processor_count(),
                    exec_graph.link_count(),
                    states,
                )
            } else {
                (0, 0, HashMap::new())
            };

        RuntimeStatus {
            running: self.state == ExecutorState::Running,
            processor_count,
            connection_count,
            processor_states,
        }
    }

    /// Get the next processor ID
    pub fn next_processor_id(&mut self) -> ProcessorId {
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;
        id
    }

    /// Get the next connection ID
    pub fn next_connection_id(&mut self) -> LinkId {
        let id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;
        // Internal use - format is guaranteed valid (alphanumeric + underscore)
        crate::core::link_channel::link_id::__private::new_unchecked(id)
    }

    /// Add a processor (alias for add_pending_processor)
    pub fn add_processor(&mut self, id: ProcessorId, processor: BoxedProcessor) {
        self.add_pending_processor(id, processor);
    }

    /// Queue a connection to be wired on start
    pub fn queue_connection(&mut self, _id: LinkId, from_port: String, to_port: String) {
        self.connections_to_wire.push((from_port, to_port));
        self.dirty = true;
    }

    /// Remove a processor (shutdown if running)
    pub fn remove_processor(&mut self, processor_id: &str) -> crate::core::Result<()> {
        let proc_id = processor_id.to_string();

        // If running, shutdown the processor
        if let Some(exec_graph) = &self.execution_graph {
            if let Some(instance) = exec_graph.get_processor_runtime(&proc_id) {
                let current_state = *instance.state.lock();
                if current_state == ProcessorState::Running {
                    self.shutdown_processor(&proc_id)?;
                }
            }
        }

        // Remove from execution graph (if present)
        if let Some(exec_graph) = &mut self.execution_graph {
            // Find and remove associated links first
            let link_ids: Vec<_> = exec_graph
                .iter_link_runtime()
                .filter(|(_, wired)| {
                    wired.source_processor() == proc_id || wired.dest_processor() == proc_id
                })
                .map(|(id, _)| id.clone())
                .collect();

            for link_id in link_ids {
                exec_graph.remove_link_runtime(&link_id);
            }
            exec_graph.remove_processor_runtime(&proc_id);
        }

        // Remove from pending if present
        self.pending_processors.retain(|(id, _)| id != processor_id);

        self.dirty = true;
        Ok(())
    }

    /// Add a processor to pending (to be started on compile/start)
    pub fn add_pending_processor(&mut self, id: ProcessorId, processor: BoxedProcessor) {
        self.pending_processors.push((id, processor));
        self.dirty = true;
    }

    /// Get running processor (for wiring)
    pub(crate) fn get_processor(&self, id: &ProcessorId) -> Option<&RunningProcessor> {
        self.execution_graph.as_ref()?.get_processor_runtime(id)
    }

    /// Get mutable running processor
    pub(crate) fn get_processor_mut(&mut self, id: &ProcessorId) -> Option<&mut RunningProcessor> {
        self.execution_graph.as_mut()?.get_processor_runtime_mut(id)
    }

    /// Get wired link
    pub(crate) fn get_connection(&self, id: &LinkId) -> Option<&WiredLink> {
        self.execution_graph.as_ref()?.get_link_runtime(id)
    }

    /// Get connections for a processor (links where processor is source or destination)
    pub fn get_processor_connections(&self, processor_id: &ProcessorId) -> Vec<LinkId> {
        let Some(exec_graph) = &self.execution_graph else {
            return Vec::new();
        };

        // Find all links connected to this processor by iterating link runtime state
        exec_graph
            .iter_link_runtime()
            .filter(|(_, wired)| {
                wired.source_processor() == processor_id || wired.dest_processor() == processor_id
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Mark executor as dirty (needs recompile)
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    // =========================================================================
    // Processor Instance Management
    // =========================================================================

    /// Convert a graph node to a running processor instance
    ///
    /// This is `to_processor_instance` from the design doc.
    /// Looks up the ProcessorNode from the Graph (DOM), spawns a thread,
    /// and creates a RunningProcessor in the ExecutionGraph (VDOM).
    fn spawn_processor(&mut self, id: ProcessorId, processor: BoxedProcessor) -> Result<()> {
        let ctx = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Runtime context not initialized".into()))?;

        // Look up the ProcessorNode from the Graph (DOM)
        let node = {
            let graph = self
                .graph
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))?;
            let graph_guard = graph.read();
            graph_guard.get_processor(&id).cloned().ok_or_else(|| {
                StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", id))
            })?
        };

        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<LinkWakeupEvent>();

        let state = Arc::new(Mutex::new(ProcessorState::Running));
        let processor_arc = Arc::new(Mutex::new(processor));

        // Setup processor with runtime context
        {
            let mut guard = processor_arc.lock();
            guard.__generated_setup(ctx)?;
        }

        let processor_clone = Arc::clone(&processor_arc);
        let state_clone = Arc::clone(&state);
        let id_clone = id.clone();

        let sched_config = processor_arc.lock().scheduling_config();

        let thread = std::thread::Builder::new()
            .name(format!("processor-{}", id))
            .spawn(move || {
                Self::processor_thread_loop(
                    id_clone,
                    processor_clone,
                    shutdown_rx,
                    wakeup_rx,
                    state_clone,
                    sched_config,
                );
            })
            .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

        // Create RunningProcessor extending the node from the graph
        let running = RunningProcessor::new(
            node,
            Some(thread),
            shutdown_tx,
            wakeup_tx,
            state,
            Some(processor_arc),
        );

        // Insert into execution graph (VDOM)
        let exec_graph = self
            .execution_graph
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;
        exec_graph.insert_processor_runtime(id, running);

        Ok(())
    }

    /// Shutdown a processor instance
    fn shutdown_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        let exec_graph = self
            .execution_graph
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;

        let instance = exec_graph
            .get_processor_runtime_mut(processor_id)
            .ok_or_else(|| {
                StreamError::NotFound(format!("Processor '{}' not found", processor_id))
            })?;

        let current_state = *instance.state.lock();
        if current_state == ProcessorState::Stopped || current_state == ProcessorState::Stopping {
            return Ok(()); // Already stopped/stopping
        }

        *instance.state.lock() = ProcessorState::Stopping;

        tracing::info!("[{}] Shutting down processor...", processor_id);

        instance.shutdown_tx.send(()).map_err(|_| {
            StreamError::Runtime(format!(
                "Failed to send shutdown signal to processor '{}'",
                processor_id
            ))
        })?;

        if let Some(handle) = instance.thread.take() {
            match handle.join() {
                Ok(_) => {
                    tracing::info!("[{}] Processor thread joined successfully", processor_id);
                    *instance.state.lock() = ProcessorState::Stopped;
                }
                Err(panic_err) => {
                    tracing::error!(
                        "[{}] Processor thread panicked: {:?}",
                        processor_id,
                        panic_err
                    );
                    *instance.state.lock() = ProcessorState::Stopped;
                    return Err(StreamError::Runtime(format!(
                        "Processor '{}' thread panicked",
                        processor_id
                    )));
                }
            }
        }

        tracing::info!("[{}] Processor shut down", processor_id);
        Ok(())
    }

    /// Processor thread main loop
    fn processor_thread_loop(
        id: ProcessorId,
        processor: Arc<Mutex<BoxedProcessor>>,
        shutdown_rx: crossbeam_channel::Receiver<()>,
        wakeup_rx: crossbeam_channel::Receiver<LinkWakeupEvent>,
        state: Arc<Mutex<ProcessorState>>,
        sched_config: SchedulingConfig,
    ) {
        tracing::debug!("[{}] Thread started with mode {:?}", id, sched_config.mode);

        match sched_config.mode {
            SchedulingMode::Loop => {
                Self::run_loop_mode(&id, &processor, &shutdown_rx);
            }
            SchedulingMode::Push => {
                Self::run_push_mode(&id, &processor, &shutdown_rx, &wakeup_rx);
            }
            SchedulingMode::Pull => {
                Self::run_pull_mode(&id, &processor, &shutdown_rx, &wakeup_rx);
            }
        }

        // Teardown
        {
            let mut guard = processor.lock();
            if let Err(e) = guard.__generated_teardown() {
                tracing::warn!("[{}] Teardown error: {}", id, e);
            }
        }

        *state.lock() = ProcessorState::Stopped;
        tracing::debug!("[{}] Thread stopped", id);
    }

    /// Loop scheduling mode - tight polling loop
    fn run_loop_mode(
        id: &ProcessorId,
        processor: &Arc<Mutex<BoxedProcessor>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
    ) {
        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            {
                let mut guard = processor.lock();
                if let Err(e) = guard.process() {
                    tracing::warn!("[{}] Process error: {}", id, e);
                }
            }

            std::thread::sleep(std::time::Duration::from_micros(10));
        }
    }

    /// Push scheduling mode - event-driven, woken on input data
    fn run_push_mode(
        id: &ProcessorId,
        processor: &Arc<Mutex<BoxedProcessor>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        wakeup_rx: &crossbeam_channel::Receiver<LinkWakeupEvent>,
    ) {
        loop {
            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(wakeup_rx) -> msg => {
                    if let Ok(event) = msg {
                        if event == LinkWakeupEvent::Shutdown {
                            break;
                        }
                        let mut guard = processor.lock();
                        if let Err(e) = guard.process() {
                            tracing::warn!("[{}] Process error: {}", id, e);
                        }
                    }
                }
            }
        }
    }

    /// Pull scheduling mode - processor manages its own callbacks
    fn run_pull_mode(
        id: &ProcessorId,
        processor: &Arc<Mutex<BoxedProcessor>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        wakeup_rx: &crossbeam_channel::Receiver<LinkWakeupEvent>,
    ) {
        // Initial process call
        {
            let mut guard = processor.lock();
            if let Err(e) = guard.process() {
                tracing::warn!("[{}] Initial process error: {}", id, e);
            }
        }

        loop {
            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(wakeup_rx) -> msg => {
                    if let Ok(event) = msg {
                        if event == LinkWakeupEvent::Shutdown {
                            break;
                        }
                    }
                }
                default(std::time::Duration::from_millis(100)) => {}
            }
        }
    }

    // =========================================================================
    // Connection Instance Management
    // =========================================================================

    /// Convert a graph edge to a live connection instance
    ///
    /// This is `to_connection_instance` from the design doc.
    /// Looks up the Link from the Graph (DOM), creates the ring buffer,
    /// and creates a WiredLink in the ExecutionGraph (VDOM).
    fn wire_connection(&mut self, from_port: &str, to_port: &str) -> Result<LinkId> {
        let (source_proc_id, source_port) = from_port.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid source format '{}'. Expected 'processor_id.port_name'",
                from_port
            ))
        })?;

        let (dest_proc_id, dest_port) = to_port.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid destination format '{}'. Expected 'processor_id.port_name'",
                to_port
            ))
        })?;

        // Look up the Link from the Graph (DOM)
        let link = {
            let graph = self
                .graph
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))?;
            let graph_guard = graph.read();
            let link_id = graph_guard.find_link(from_port, to_port).ok_or_else(|| {
                StreamError::InvalidLink(format!(
                    "Link '{}' → '{}' not found in graph",
                    from_port, to_port
                ))
            })?;
            graph_guard
                .find_link_by_id(&link_id)
                .cloned()
                .ok_or_else(|| {
                    StreamError::InvalidLink(format!("Link '{}' not found by ID", link_id))
                })?
        };

        let connection_id = link.id.clone();

        tracing::info!(
            "Wiring {} ({}:{}) → ({}:{}) [{}]",
            from_port,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Get processor references from execution graph
        let exec_graph = self
            .execution_graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;

        let (source_processor, dest_processor) = {
            let source_instance = exec_graph
                .get_processor_runtime(&source_proc_id.to_string())
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' not found",
                        source_proc_id
                    ))
                })?;

            let dest_instance = exec_graph
                .get_processor_runtime(&dest_proc_id.to_string())
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Destination processor '{}' not found",
                        dest_proc_id
                    ))
                })?;

            let source_proc = source_instance.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Source processor '{}' has no processor reference",
                    source_proc_id
                ))
            })?;

            let dest_proc = dest_instance.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Destination processor '{}' has no processor reference",
                    dest_proc_id
                ))
            })?;

            (Arc::clone(source_proc), Arc::clone(dest_proc))
        };

        // Validate audio requirements
        {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            let source_descriptor = source_guard.descriptor_instance();
            let dest_descriptor = dest_guard.descriptor_instance();

            if let (Some(source_desc), Some(dest_desc)) = (source_descriptor, dest_descriptor) {
                if let (Some(source_audio), Some(dest_audio)) = (
                    &source_desc.audio_requirements,
                    &dest_desc.audio_requirements,
                ) {
                    if !source_audio.compatible_with(dest_audio) {
                        let error_msg = source_audio.compatibility_error(dest_audio);
                        return Err(StreamError::Configuration(format!(
                            "Audio requirements incompatible: {} → {}: {}",
                            from_port, to_port, error_msg
                        )));
                    }
                }
            }
        }

        // Get port types and validate compatibility
        let (source_port_type, dest_port_type) = {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            let src_type = source_guard
                .get_output_port_type(source_port)
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' does not have output port '{}'",
                        source_proc_id, source_port
                    ))
                })?;

            let dst_type = dest_guard.get_input_port_type(dest_port).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Destination processor '{}' does not have input port '{}'",
                    dest_proc_id, dest_port
                ))
            })?;

            if !src_type.compatible_with(&dst_type) {
                return Err(StreamError::Configuration(format!(
                    "Port type mismatch: {} ({:?}) → {} ({:?})",
                    from_port, src_type, to_port, dst_type
                )));
            }

            (src_type, dst_type)
        };

        // Create port addresses and determine capacity
        let source_addr = LinkPortAddress::new(source_proc_id.to_string(), source_port.to_string());
        let dest_addr = LinkPortAddress::new(dest_proc_id.to_string(), dest_port.to_string());
        let capacity = source_port_type.default_capacity();

        // Wire connection based on port type
        self.wire_by_port_type(
            source_port_type,
            &source_addr,
            &dest_addr,
            capacity,
            &source_processor,
            &dest_processor,
            source_port,
            dest_port,
        )?;

        tracing::info!(
            "Wired {} ({:?}) → {} ({:?}) via rtrb",
            from_port,
            source_port_type,
            to_port,
            dest_port_type
        );

        // Create WiredLink extending the Link from the graph
        let wired = WiredLink::new(link, source_port_type, capacity);

        // Wire wakeup channel for push/pull scheduling
        {
            let exec_graph = self
                .execution_graph
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;

            let source_instance = exec_graph.get_processor_runtime(&source_proc_id.to_string());
            let dest_instance = exec_graph.get_processor_runtime(&dest_proc_id.to_string());

            if let (Some(src), Some(dst)) = (source_instance, dest_instance) {
                if let Some(src_proc) = src.processor.as_ref() {
                    let mut source_guard = src_proc.lock();
                    source_guard.set_output_wakeup(source_port, dst.wakeup_tx.clone());

                    tracing::debug!(
                        "Wired wakeup notification: {} ({}) → {} ({})",
                        source_proc_id,
                        source_port,
                        dest_proc_id,
                        dest_port
                    );
                }
            }
        }

        // Insert WiredLink into execution graph (VDOM)
        let exec_graph = self
            .execution_graph
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;
        exec_graph.insert_link_runtime(connection_id.clone(), wired);

        tracing::info!("Registered connection: {}", connection_id);
        Ok(connection_id)
    }

    /// Wire connection based on port type (creates appropriate ring buffer)
    fn wire_by_port_type(
        &mut self,
        port_type: LinkPortType,
        source_addr: &LinkPortAddress,
        dest_addr: &LinkPortAddress,
        capacity: usize,
        source_processor: &Arc<Mutex<BoxedProcessor>>,
        dest_processor: &Arc<Mutex<BoxedProcessor>>,
        source_port: &str,
        dest_port: &str,
    ) -> Result<()> {
        match port_type {
            LinkPortType::Audio => {
                let (producer, consumer) = self.link_channel.create_channel::<AudioFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                if !source_guard.wire_output_producer(source_port, Box::new(producer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}'",
                        source_port
                    )));
                }
                drop(source_guard);

                let mut dest_guard = dest_processor.lock();
                if !dest_guard.wire_input_consumer(dest_port, Box::new(consumer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}'",
                        dest_port
                    )));
                }
            }
            LinkPortType::Video => {
                let (producer, consumer) = self.link_channel.create_channel::<VideoFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                if !source_guard.wire_output_producer(source_port, Box::new(producer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}'",
                        source_port
                    )));
                }
                drop(source_guard);

                let mut dest_guard = dest_processor.lock();
                if !dest_guard.wire_input_consumer(dest_port, Box::new(consumer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}'",
                        dest_port
                    )));
                }
            }
            LinkPortType::Data => {
                let (producer, consumer) = self.link_channel.create_channel::<DataFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                if !source_guard.wire_output_producer(source_port, Box::new(producer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}'",
                        source_port
                    )));
                }
                drop(source_guard);

                let mut dest_guard = dest_processor.lock();
                if !dest_guard.wire_input_consumer(dest_port, Box::new(consumer)) {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}'",
                        dest_port
                    )));
                }
            }
        }
        Ok(())
    }

    /// Unwire a connection
    #[allow(dead_code)]
    fn unwire_connection(&mut self, connection_id: &LinkId) -> Result<()> {
        let exec_graph = self
            .execution_graph
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))?;

        tracing::info!("Unwiring connection: {}", connection_id);

        // TODO: Full port-level cleanup (remove producers/consumers from processors)
        tracing::warn!(
            "Connection {} unwired from tracking, but port-level cleanup not yet implemented",
            connection_id
        );

        // Remove from execution graph (handles index cleanup internally)
        exec_graph.remove_link_runtime(connection_id);

        tracing::info!("Unwired connection: {}", connection_id);
        Ok(())
    }

    /// Send initialization wakeup to Pull mode processors
    fn send_pull_mode_wakeups(&self) {
        tracing::debug!("Sending initialization wakeup to Pull mode processors");

        let Some(exec_graph) = &self.execution_graph else {
            tracing::warn!("Cannot send wakeups: execution graph not initialized");
            return;
        };

        for (proc_id, instance) in exec_graph.iter_processor_runtime() {
            if let Some(proc_ref) = &instance.processor {
                let sched_config = proc_ref.lock().scheduling_config();
                if matches!(sched_config.mode, SchedulingMode::Pull) {
                    if let Err(e) = instance.wakeup_tx.send(LinkWakeupEvent::DataAvailable) {
                        tracing::warn!(
                            "[{}] Failed to send Pull mode initialization wakeup: {}",
                            proc_id,
                            e
                        );
                    } else {
                        tracing::debug!("[{}] Sent Pull mode initialization wakeup", proc_id);
                    }
                }
            }
        }
    }

    /// Run the default event loop (blocking)
    ///
    /// This blocks until a shutdown signal is received (SIGTERM, SIGINT, or Ctrl+C).
    /// The event loop subscribes to shutdown events from the global event bus.
    fn run_event_loop(&self) -> Result<()> {
        use crate::core::pubsub::{Event, EventListener, RuntimeEvent, EVENT_BUS};

        tracing::info!("Running event loop (waiting for shutdown signal)...");

        // Create a channel to receive shutdown notification
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded::<()>(1);

        // Create a listener that forwards shutdown events to the channel
        struct ShutdownListener {
            tx: crossbeam_channel::Sender<()>,
        }

        impl EventListener for ShutdownListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if matches!(event, Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown)) {
                    let _ = self.tx.send(());
                }
                Ok(())
            }
        }

        // Subscribe to runtime shutdown events
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(ShutdownListener { tx: shutdown_tx }));
        EVENT_BUS.subscribe("runtime:global", listener);

        // Block until shutdown signal received
        match shutdown_rx.recv() {
            Ok(()) => {
                tracing::info!("Shutdown signal received, stopping event loop");
            }
            Err(_) => {
                // Channel closed - this shouldn't happen but treat as shutdown
                tracing::warn!("Event loop channel closed unexpectedly");
            }
        }

        Ok(())
    }
}

impl Executor for SimpleExecutor {
    fn state(&self) -> ExecutorState {
        self.state
    }

    fn compile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()> {
        tracing::info!("Compiling graph...");

        self.runtime_context = Some(Arc::new(ctx.clone()));

        // Validate graph (checks for cycles, etc.)
        graph.validate()?;

        self.dirty = false;
        self.state = ExecutorState::Compiled;

        tracing::info!("Graph compiled successfully");
        Ok(())
    }

    fn recompile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()> {
        // For now, just recompile from scratch
        // TODO: Implement delta-based recompilation
        self.compile(graph, ctx)
    }

    fn start(&mut self) -> Result<()> {
        // Auto-compile if in Idle state
        if self.state == ExecutorState::Idle {
            self.compile_from_graph()?;
        }

        if self.state != ExecutorState::Compiled {
            return Err(StreamError::Runtime(format!(
                "Cannot start executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Starting executor...");

        // Spawn all pending processors
        let pending = std::mem::take(&mut self.pending_processors);
        for (id, processor) in pending {
            self.spawn_processor(id, processor)?;
        }

        // Wire all connections from execution plan
        // Note: connections_to_wire is populated during compile from the graph
        for (from_port, to_port) in std::mem::take(&mut self.connections_to_wire) {
            self.wire_connection(&from_port, &to_port)?;
        }

        // Send initialization wakeup to Pull mode processors
        self.send_pull_mode_wakeups();

        self.state = ExecutorState::Running;
        tracing::info!("Executor started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.state != ExecutorState::Running && self.state != ExecutorState::Paused {
            return Err(StreamError::Runtime(format!(
                "Cannot stop executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Stopping executor...");

        // Shutdown all processors in the execution graph
        let processor_ids: Vec<_> = self
            .execution_graph
            .as_ref()
            .map(|eg| eg.processor_ids().cloned().collect())
            .unwrap_or_default();

        for id in processor_ids {
            if let Err(e) = self.shutdown_processor(&id) {
                tracing::warn!("Error shutting down processor {}: {}", id, e);
            }
        }

        // Clear execution graph
        if let Some(exec_graph) = &mut self.execution_graph {
            exec_graph.clear_runtime_state();
        }

        self.state = ExecutorState::Idle;
        tracing::info!("Executor stopped");
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        if self.state != ExecutorState::Running {
            return Err(StreamError::Runtime(format!(
                "Cannot pause executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Pausing executor...");
        // TODO: Implement actual pause (signal processors to suspend)
        self.state = ExecutorState::Paused;
        tracing::info!("Executor paused");
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.state != ExecutorState::Paused {
            return Err(StreamError::Runtime(format!(
                "Cannot resume executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Resuming executor...");
        // TODO: Implement actual resume (signal processors to continue)
        self.state = ExecutorState::Running;
        tracing::info!("Executor resumed");
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        // Start if not already running (start() auto-compiles if needed)
        if self.state != ExecutorState::Running {
            self.start()?;
        }

        // Install signal handlers
        crate::core::signals::install_signal_handlers().map_err(|e| {
            StreamError::Configuration(format!("Failed to install signal handlers: {}", e))
        })?;

        // Run the default event loop - blocking until shutdown
        self.run_event_loop()?;

        // Stop and cleanup
        self.stop()?;
        Ok(())
    }

    fn needs_recompile(&self) -> bool {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_creation() {
        let executor = SimpleExecutor::new();
        assert_eq!(executor.state(), ExecutorState::Idle);
        assert!(!executor.needs_recompile());
    }

    #[test]
    fn test_wired_link_parsing() {
        use crate::core::graph::Link;
        use crate::core::link_channel::link_id::__private::new_unchecked;

        let link = Link::new(new_unchecked("conn_0"), "proc_a.video", "proc_b.video");
        let wired = WiredLink::new(link, LinkPortType::Video, 16);

        assert_eq!(wired.source_processor(), "proc_a");
        assert_eq!(wired.dest_processor(), "proc_b");
        // Deref allows direct access to Link fields
        assert_eq!(wired.id.as_str(), "conn_0");
    }
}
