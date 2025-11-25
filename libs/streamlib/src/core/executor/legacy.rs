//! Legacy executor implementation
//!
//! This executor implements the original streamlib execution model:
//! - One thread per processor
//! - Lock-free ring buffer connections (via rtrb)
//! - Three scheduling modes: Loop, Push, Pull
//!
//! # Execution Model
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     LegacyExecutor                          │
//! │                                                             │
//! │  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐     │
//! │  │  Thread 1   │    │  Thread 2   │    │  Thread 3   │     │
//! │  │ Processor A │───►│ Processor B │───►│ Processor C │     │
//! │  └─────────────┘    └─────────────┘    └─────────────┘     │
//! │         │                  │                  │             │
//! │         └──────────────────┴──────────────────┘             │
//! │                   Lock-free ring buffers                    │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::{Mutex, RwLock};

use super::{Executor, ExecutorState};
use crate::core::bus::{Bus, ConnectionId, PortAddress, PortType, WakeupEvent};
use crate::core::context::{GpuContext, RuntimeContext};
use crate::core::error::{Result, StreamError};
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{Graph, ProcessorId};
use crate::core::graph_optimizer::{ExecutionPlan, GraphOptimizer};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode};
use crate::core::traits::DynStreamElement;

/// Type alias for boxed dynamic processor
pub type DynProcessor = Box<dyn DynStreamElement + Send>;

/// Status of a processor instance
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    /// Waiting to be started
    Pending,
    /// Currently running
    Running,
    /// In process of stopping
    Stopping,
    /// Stopped
    Stopped,
}

/// Runtime status snapshot
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// Whether the runtime is currently running
    pub running: bool,
    /// Number of processors registered
    pub processor_count: usize,
    /// Number of active connections
    pub connection_count: usize,
    /// Per-processor status
    pub processor_statuses: HashMap<ProcessorId, ProcessorStatus>,
}

/// Runtime handle for a processor instance
pub(crate) struct ProcessorInstance {
    #[allow(dead_code)]
    pub id: ProcessorId,
    pub thread: Option<JoinHandle<()>>,
    pub shutdown_tx: crossbeam_channel::Sender<()>,
    pub wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    pub status: Arc<Mutex<ProcessorStatus>>,
    pub processor: Option<Arc<Mutex<DynProcessor>>>,
}

/// Runtime metadata for a connection instance
#[derive(Debug, Clone)]
pub struct ConnectionInstance {
    pub id: ConnectionId,
    pub from_port: String,
    pub to_port: String,
    pub port_type: PortType,
    pub capacity: usize,
    pub source_processor: ProcessorId,
    pub dest_processor: ProcessorId,
}

impl ConnectionInstance {
    pub fn new(
        id: ConnectionId,
        from_port: String,
        to_port: String,
        port_type: PortType,
        capacity: usize,
    ) -> Self {
        let source_processor = from_port
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string();
        let dest_processor = to_port.split('.').next().unwrap_or_default().to_string();

        Self {
            id,
            from_port,
            to_port,
            port_type,
            capacity,
            source_processor,
            dest_processor,
        }
    }
}

/// Legacy executor - thread-per-processor with lock-free connections
pub struct LegacyExecutor {
    state: ExecutorState,
    /// Shared graph (DOM) - read-only access to topology
    graph: Option<Arc<RwLock<Graph>>>,
    /// Runtime context (GPU + main thread dispatch)
    runtime_context: Option<Arc<RuntimeContext>>,
    /// Live processor instances
    processors: HashMap<ProcessorId, ProcessorInstance>,
    /// Processors waiting to be started (from runtime's pending_processors)
    pending_processors: Vec<(ProcessorId, DynProcessor)>,
    /// Live connection instances
    connections: HashMap<ConnectionId, ConnectionInstance>,
    /// Connection bus (creates ring buffers)
    bus: Bus,
    /// Index: processor ID → connection IDs
    processor_connections: HashMap<ProcessorId, Vec<ConnectionId>>,
    /// Graph optimizer
    optimizer: GraphOptimizer,
    /// Current execution plan
    execution_plan: Option<ExecutionPlan>,
    /// Connections to wire on start (from_port, to_port)
    connections_to_wire: Vec<(String, String)>,
    /// Next processor ID counter
    next_processor_id: usize,
    /// Next connection ID counter
    next_connection_id: usize,
    /// Graph has changed since last compile
    dirty: bool,
}

impl Default for LegacyExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl LegacyExecutor {
    /// Create a new legacy executor (standalone, no shared graph)
    pub fn new() -> Self {
        Self {
            state: ExecutorState::Idle,
            graph: None,
            runtime_context: None,
            processors: HashMap::new(),
            pending_processors: Vec::new(),
            connections: HashMap::new(),
            bus: Bus::new(),
            processor_connections: HashMap::new(),
            optimizer: GraphOptimizer::new(),
            execution_plan: None,
            connections_to_wire: Vec::new(),
            next_processor_id: 0,
            next_connection_id: 0,
            dirty: false,
        }
    }

    /// Create a new legacy executor with a shared graph reference
    ///
    /// The executor reads the graph on compile/start to understand the topology.
    /// The runtime modifies the graph, and the executor sees changes via the shared reference.
    pub fn with_graph(graph: Arc<RwLock<Graph>>) -> Self {
        Self {
            state: ExecutorState::Idle,
            graph: Some(graph),
            runtime_context: None,
            processors: HashMap::new(),
            pending_processors: Vec::new(),
            connections: HashMap::new(),
            bus: Bus::new(),
            processor_connections: HashMap::new(),
            optimizer: GraphOptimizer::new(),
            execution_plan: None,
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
    pub fn register_processor(&mut self, id: ProcessorId, processor: DynProcessor) {
        self.pending_processors.push((id, processor));
        self.dirty = true;
    }

    /// Compile the execution plan from the shared graph
    ///
    /// This is an internal method that reads the shared graph, creates a runtime context,
    /// and compiles an execution plan. Called automatically by start() if needed.
    fn compile_from_graph(&mut self) -> Result<()> {
        // Get the graph (either shared or we need one passed in)
        let graph = self
            .graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))?;

        // Read graph to extract connections
        let graph_guard = graph.read();

        // Validate graph
        graph_guard.validate()?;

        // Extract connections to wire from the graph edges
        self.connections_to_wire.clear();
        for edge in graph_guard.petgraph().edge_indices() {
            let edge_data = &graph_guard.petgraph()[edge];
            self.connections_to_wire
                .push((edge_data.from_port.clone(), edge_data.to_port.clone()));
        }

        // Generate execution plan
        let plan = self.optimizer.optimize(&graph_guard)?;
        tracing::debug!("Execution plan: {:?}", plan);
        self.execution_plan = Some(plan);

        // Drop the guard before creating context (might need GPU init)
        drop(graph_guard);

        // Create runtime context (GPU + main thread dispatch)
        let gpu_context = GpuContext::init_for_platform_sync()?;
        let runtime_context = RuntimeContext::new(gpu_context);
        self.runtime_context = Some(Arc::new(runtime_context));

        self.dirty = false;
        self.state = ExecutorState::Compiled;

        tracing::info!("Execution plan compiled successfully");
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

    /// Get execution plan
    pub fn execution_plan(&self) -> Option<&ExecutionPlan> {
        self.execution_plan.as_ref()
    }

    /// Get runtime status snapshot
    pub fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            running: self.state == ExecutorState::Running,
            processor_count: self.processors.len(),
            connection_count: self.connections.len(),
            processor_statuses: self
                .processors
                .iter()
                .map(|(id, inst)| (id.clone(), *inst.status.lock()))
                .collect(),
        }
    }

    /// Get the next processor ID
    pub fn next_processor_id(&mut self) -> ProcessorId {
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;
        id
    }

    /// Get the next connection ID
    pub fn next_connection_id(&mut self) -> ConnectionId {
        let id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;
        // Internal use - format is guaranteed valid (alphanumeric + underscore)
        crate::core::bus::connection_id::__private::new_unchecked(id)
    }

    /// Add a processor (alias for add_pending_processor)
    pub fn add_processor(&mut self, id: ProcessorId, processor: DynProcessor) {
        self.add_pending_processor(id, processor);
    }

    /// Queue a connection to be wired on start
    pub fn queue_connection(&mut self, _id: ConnectionId, from_port: String, to_port: String) {
        self.connections_to_wire.push((from_port, to_port));
        self.dirty = true;
    }

    /// Remove a processor (shutdown if running)
    pub fn remove_processor(&mut self, processor_id: &str) -> crate::core::Result<()> {
        let proc_id = processor_id.to_string();

        // If running, shutdown the processor
        if let Some(instance) = self.processors.get(&proc_id) {
            let status = *instance.status.lock();
            if status == ProcessorStatus::Running {
                self.shutdown_processor(&proc_id)?;
            }
        }

        // Remove from processors map
        self.processors.remove(&proc_id);

        // Remove from pending if present
        self.pending_processors.retain(|(id, _)| id != processor_id);

        // Remove associated connections
        if let Some(conn_ids) = self.processor_connections.remove(&proc_id) {
            for conn_id in conn_ids {
                self.connections.remove(&conn_id);
            }
        }

        self.dirty = true;
        Ok(())
    }

    /// Add a processor to pending (to be started on compile/start)
    pub fn add_pending_processor(&mut self, id: ProcessorId, processor: DynProcessor) {
        self.pending_processors.push((id, processor));
        self.dirty = true;
    }

    /// Get processor instance (for wiring)
    pub(crate) fn get_processor(&self, id: &ProcessorId) -> Option<&ProcessorInstance> {
        self.processors.get(id)
    }

    /// Get mutable processor instance
    pub(crate) fn get_processor_mut(&mut self, id: &ProcessorId) -> Option<&mut ProcessorInstance> {
        self.processors.get_mut(id)
    }

    /// Get connection instance
    pub fn get_connection(&self, id: &ConnectionId) -> Option<&ConnectionInstance> {
        self.connections.get(id)
    }

    /// Get connections for a processor
    pub fn get_processor_connections(&self, processor_id: &ProcessorId) -> Vec<ConnectionId> {
        self.processor_connections
            .get(processor_id)
            .cloned()
            .unwrap_or_default()
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
    /// It spawns a thread and sets up the processor for execution.
    fn spawn_processor(&mut self, id: ProcessorId, processor: DynProcessor) -> Result<()> {
        let ctx = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Runtime context not initialized".into()))?;

        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

        let status = Arc::new(Mutex::new(ProcessorStatus::Running));
        let processor_arc = Arc::new(Mutex::new(processor));

        // Setup processor with runtime context
        {
            let mut guard = processor_arc.lock();
            guard.__generated_setup(ctx)?;
        }

        let processor_clone = Arc::clone(&processor_arc);
        let status_clone = Arc::clone(&status);
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
                    status_clone,
                    sched_config,
                );
            })
            .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

        let instance = ProcessorInstance {
            id: id.clone(),
            thread: Some(thread),
            shutdown_tx,
            wakeup_tx,
            status,
            processor: Some(processor_arc),
        };

        self.processors.insert(id, instance);
        Ok(())
    }

    /// Shutdown a processor instance
    fn shutdown_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        let instance = self.processors.get_mut(processor_id).ok_or_else(|| {
            StreamError::NotFound(format!("Processor '{}' not found", processor_id))
        })?;

        let current_status = *instance.status.lock();
        if current_status == ProcessorStatus::Stopped
            || current_status == ProcessorStatus::Stopping
        {
            return Ok(()); // Already stopped/stopping
        }

        *instance.status.lock() = ProcessorStatus::Stopping;

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
                    *instance.status.lock() = ProcessorStatus::Stopped;
                }
                Err(panic_err) => {
                    tracing::error!(
                        "[{}] Processor thread panicked: {:?}",
                        processor_id,
                        panic_err
                    );
                    *instance.status.lock() = ProcessorStatus::Stopped;
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
        processor: Arc<Mutex<DynProcessor>>,
        shutdown_rx: crossbeam_channel::Receiver<()>,
        wakeup_rx: crossbeam_channel::Receiver<WakeupEvent>,
        status: Arc<Mutex<ProcessorStatus>>,
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

        *status.lock() = ProcessorStatus::Stopped;
        tracing::debug!("[{}] Thread stopped", id);
    }

    /// Loop scheduling mode - tight polling loop
    fn run_loop_mode(
        id: &ProcessorId,
        processor: &Arc<Mutex<DynProcessor>>,
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
        processor: &Arc<Mutex<DynProcessor>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        wakeup_rx: &crossbeam_channel::Receiver<WakeupEvent>,
    ) {
        loop {
            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(wakeup_rx) -> msg => {
                    if let Ok(event) = msg {
                        if event == WakeupEvent::Shutdown {
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
        processor: &Arc<Mutex<DynProcessor>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        wakeup_rx: &crossbeam_channel::Receiver<WakeupEvent>,
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
                        if event == WakeupEvent::Shutdown {
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
    /// It creates the ring buffer and wires producer/consumer to ports.
    fn wire_connection(&mut self, from_port: &str, to_port: &str) -> Result<ConnectionId> {
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

        // Generate connection ID
        let connection_id = self.next_connection_id();

        tracing::info!(
            "Wiring {} ({}:{}) → ({}:{}) [{}]",
            from_port,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Get processor references
        let (source_processor, dest_processor) = {
            let source_instance = self.processors.get(source_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Source processor '{}' not found",
                    source_proc_id
                ))
            })?;

            let dest_instance = self.processors.get(dest_proc_id).ok_or_else(|| {
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
        let source_addr = PortAddress::new(source_proc_id.to_string(), source_port.to_string());
        let dest_addr = PortAddress::new(dest_proc_id.to_string(), dest_port.to_string());
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

        // Store connection metadata
        let connection = ConnectionInstance::new(
            connection_id.clone(),
            from_port.to_string(),
            to_port.to_string(),
            source_port_type,
            capacity,
        );

        // Update processor connections index
        self.processor_connections
            .entry(connection.source_processor.clone())
            .or_default()
            .push(connection_id.clone());

        self.processor_connections
            .entry(connection.dest_processor.clone())
            .or_default()
            .push(connection_id.clone());

        self.connections
            .insert(connection_id.clone(), connection);

        // Wire wakeup channel for push/pull scheduling
        {
            let source_instance = self.processors.get(source_proc_id);
            let dest_instance = self.processors.get(dest_proc_id);

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

        tracing::info!("Registered connection: {}", connection_id);
        Ok(connection_id)
    }

    /// Wire connection based on port type (creates appropriate ring buffer)
    fn wire_by_port_type(
        &mut self,
        port_type: PortType,
        source_addr: &PortAddress,
        dest_addr: &PortAddress,
        capacity: usize,
        source_processor: &Arc<Mutex<DynProcessor>>,
        dest_processor: &Arc<Mutex<DynProcessor>>,
        source_port: &str,
        dest_port: &str,
    ) -> Result<()> {
        match port_type {
            PortType::Audio => {
                let (producer, consumer) = self.bus.create_connection::<AudioFrame>(
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
            PortType::Video => {
                let (producer, consumer) = self.bus.create_connection::<VideoFrame>(
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
            PortType::Data => {
                let (producer, consumer) = self.bus.create_connection::<DataFrame>(
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
    fn unwire_connection(&mut self, connection_id: &ConnectionId) -> Result<()> {
        let connection = self.connections.get(connection_id).cloned().ok_or_else(|| {
            StreamError::Configuration(format!("Connection {} not found", connection_id))
        })?;

        tracing::info!("Unwiring connection: {}", connection_id);

        // TODO: Full port-level cleanup (remove producers/consumers from processors)
        tracing::warn!(
            "Connection {} unwired from tracking, but port-level cleanup not yet implemented",
            connection_id
        );

        // Remove from connections
        self.connections.remove(connection_id);

        // Remove from processor connections index
        if let Some(connections) = self.processor_connections.get_mut(&connection.source_processor)
        {
            connections.retain(|id| id != connection_id);
        }
        if let Some(connections) = self.processor_connections.get_mut(&connection.dest_processor) {
            connections.retain(|id| id != connection_id);
        }

        tracing::info!("Unwired connection: {}", connection_id);
        Ok(())
    }

    /// Send initialization wakeup to Pull mode processors
    fn send_pull_mode_wakeups(&self) {
        tracing::debug!("Sending initialization wakeup to Pull mode processors");

        for (proc_id, instance) in &self.processors {
            if let Some(proc_ref) = &instance.processor {
                let sched_config = proc_ref.lock().scheduling_config();
                if matches!(sched_config.mode, SchedulingMode::Pull) {
                    if let Err(e) = instance.wakeup_tx.send(WakeupEvent::DataAvailable) {
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

impl Executor for LegacyExecutor {
    fn state(&self) -> ExecutorState {
        self.state
    }

    fn compile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()> {
        tracing::info!("Compiling execution plan...");

        self.runtime_context = Some(Arc::new(ctx.clone()));

        // Validate graph
        graph.validate()?;

        // Generate execution plan
        let plan = self.optimizer.optimize(graph)?;
        tracing::debug!("Execution plan: {:?}", plan);

        self.execution_plan = Some(plan);
        self.dirty = false;
        self.state = ExecutorState::Compiled;

        tracing::info!("Execution plan compiled successfully");
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

        // Shutdown all processors
        let processor_ids: Vec<_> = self.processors.keys().cloned().collect();
        for id in processor_ids {
            if let Err(e) = self.shutdown_processor(&id) {
                tracing::warn!("Error shutting down processor {}: {}", id, e);
            }
        }

        // Clear connections
        self.connections.clear();
        self.processor_connections.clear();

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
        let executor = LegacyExecutor::new();
        assert_eq!(executor.state(), ExecutorState::Idle);
        assert!(!executor.needs_recompile());
    }

    #[test]
    fn test_connection_instance_parsing() {
        use crate::core::bus::connection_id::__private::new_unchecked;

        let conn = ConnectionInstance::new(
            new_unchecked("conn_0"),
            "proc_a.video".into(),
            "proc_b.video".into(),
            PortType::Video,
            16,
        );

        assert_eq!(conn.source_processor, "proc_a");
        assert_eq!(conn.dest_processor, "proc_b");
    }
}
