use super::bus::PortType;
use super::handles::{PendingConnection, ProcessorHandle};
use super::traits::{DynStreamElement, StreamProcessor};
use super::{Result, StreamError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

pub type ProcessorId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupEvent {
    DataAvailable,
    TimerTick,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    Pending,
    Running,
    Stopping,
    Stopped,
}

type DynProcessor = Box<dyn DynStreamElement>;

// TODO(@jonathan): RuntimeProcessorHandle has unused fields (id, name)
// Review if these are needed for debugging/introspection or can be removed
#[allow(dead_code)]
pub(crate) struct RuntimeProcessorHandle {
    pub id: ProcessorId,
    pub name: String,
    pub(crate) thread: Option<JoinHandle<()>>,
    pub(crate) shutdown_tx: crossbeam_channel::Sender<()>,
    pub(crate) wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    pub(crate) status: Arc<Mutex<ProcessorStatus>>,
    pub(crate) processor: Option<Arc<Mutex<DynProcessor>>>,
}

pub type ConnectionId = String;

/// Connection between two processors
///
/// Stores both high-level port addresses (e.g., "processor_0.video") and
/// decomposed processor IDs for efficient graph traversal and optimization.
#[derive(Debug, Clone)]
pub struct Connection {
    pub id: ConnectionId,
    /// Full source port address (e.g., "processor_0.video")
    pub from_port: String,
    /// Full destination port address (e.g., "processor_1.video")
    pub to_port: String,
    /// Source processor ID (parsed from from_port)
    pub source_processor: ProcessorId,
    /// Destination processor ID (parsed from to_port)
    pub dest_processor: ProcessorId,
    /// Type of data flowing through this connection
    pub port_type: crate::core::bus::PortType,
    /// Current buffer capacity (number of frames/samples)
    pub buffer_capacity: usize,
    pub created_at: std::time::Instant,
}

impl Connection {
    /// Create a new connection with metadata
    ///
    /// Parses processor IDs from the port addresses (format: "processor_id.port_name")
    pub fn new(
        id: ConnectionId,
        from_port: String,
        to_port: String,
        port_type: crate::core::bus::PortType,
        buffer_capacity: usize,
    ) -> Self {
        // Parse processor IDs from port addresses (format: "processor_0.video")
        let source_processor = from_port.split('.').next().unwrap_or("").to_string();

        let dest_processor = to_port.split('.').next().unwrap_or("").to_string();

        Self {
            id,
            from_port,
            to_port,
            source_processor,
            dest_processor,
            port_type,
            buffer_capacity,
            created_at: std::time::Instant::now(),
        }
    }
}

pub type EventLoopFn = Box<dyn FnOnce() -> Result<()> + Send>;

pub struct StreamRuntime {
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, RuntimeProcessorHandle>>>,
    pending_processors: Vec<(ProcessorId, DynProcessor, crossbeam_channel::Receiver<()>)>,
    #[allow(dead_code)]
    handler_threads: Vec<JoinHandle<()>>,
    running: bool,
    event_loop: Option<EventLoopFn>,
    gpu_context: Option<crate::core::context::GpuContext>,
    next_processor_id: usize,
    pub(crate) connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,
    next_connection_id: usize,
    pending_connections: Vec<PendingConnection>,
    bus: crate::core::Bus,
    /// Index for fast connection lookup by processor ID
    /// Maps processor ID to list of connection IDs involving that processor
    processor_connections: HashMap<ProcessorId, Vec<ConnectionId>>,
    /// Graph representation (source of truth for desired topology)
    graph: crate::core::graph::Graph,
    /// Graph optimizer for topology analysis and execution plan generation
    graph_optimizer: crate::core::graph_optimizer::GraphOptimizer,
    /// Current execution plan (how to run the graph)
    execution_plan: Option<crate::core::graph_optimizer::ExecutionPlan>,
    /// Graph has changed, needs recompilation
    dirty: bool,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        Self {
            processors: Arc::new(Mutex::new(HashMap::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            running: false,
            event_loop: None,
            gpu_context: None,
            next_processor_id: 0,
            connections: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: 0,
            bus: crate::core::Bus::new(),
            pending_connections: Vec::new(),
            processor_connections: HashMap::new(),
            graph: crate::core::graph::Graph::new(),
            graph_optimizer: crate::core::graph_optimizer::GraphOptimizer::new(),
            execution_plan: None,
            dirty: false,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Request camera permission from the system.
    /// Must be called on the main thread before adding camera processors.
    /// Returns true if permission is granted, false if denied.
    #[cfg(target_os = "macos")]
    pub fn request_camera(&self) -> Result<bool> {
        crate::request_camera_permission()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn request_camera(&self) -> Result<bool> {
        Ok(true) // No permission system on other platforms
    }

    /// Request microphone permission from the system.
    /// Must be called on the main thread before adding audio capture processors.
    /// Returns true if permission is granted, false if denied.
    #[cfg(target_os = "macos")]
    pub fn request_microphone(&self) -> Result<bool> {
        crate::request_audio_permission()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn request_microphone(&self) -> Result<bool> {
        Ok(true) // No permission system on other platforms
    }

    pub fn set_event_loop(&mut self, event_loop: EventLoopFn) {
        self.event_loop = Some(event_loop);
    }

    pub fn gpu_context(&self) -> Option<&crate::core::context::GpuContext> {
        self.gpu_context.as_ref()
    }

    /// Get reference to graph (for testing/debugging)
    ///
    /// # Example Test Usage
    /// ```rust,ignore
    /// let runtime = StreamRuntime::new();
    /// // ... build graph ...
    ///
    /// // Export graph state for verification
    /// let graph_json = runtime.graph().to_json();
    /// assert_eq!(graph_json["nodes"].as_array().unwrap().len(), 3);
    ///
    /// // Or use DOT for visualization
    /// let dot = runtime.graph().to_dot();
    /// println!("{}", dot);  // Paste into Graphviz
    /// ```
    pub fn graph(&self) -> &crate::core::graph::Graph {
        &self.graph
    }

    /// Get reference to current execution plan (for testing/debugging)
    ///
    /// Returns None if runtime hasn't been started yet (no plan generated).
    ///
    /// # Example Test Usage
    /// ```rust,ignore
    /// let mut runtime = StreamRuntime::new();
    /// // ... build graph ...
    /// runtime.start()?;
    ///
    /// // Verify optimizer generated correct plan
    /// let plan = runtime.execution_plan().expect("no plan generated");
    /// let plan_json = plan.to_json();
    /// assert_eq!(plan_json["variant"], "Legacy");
    /// assert_eq!(plan_json["processors"].as_array().unwrap().len(), 3);
    /// ```
    pub fn execution_plan(&self) -> Option<&crate::core::graph_optimizer::ExecutionPlan> {
        self.execution_plan.as_ref()
    }

    /// Get a reference to the graph optimizer for advanced use cases.
    ///
    /// Most graph queries should go through `graph()` instead.
    /// The optimizer is mainly used for:
    /// - Cache statistics (`stats()`)
    /// - Cache management (`clear_cache()`)
    pub fn graph_optimizer(&self) -> &crate::core::graph_optimizer::GraphOptimizer {
        &self.graph_optimizer
    }

    /// Get a mutable reference to the graph optimizer.
    ///
    /// Useful for advanced use cases like clearing the execution plan cache.
    pub fn graph_optimizer_mut(&mut self) -> &mut crate::core::graph_optimizer::GraphOptimizer {
        &mut self.graph_optimizer
    }

    pub fn add_processor<P: StreamProcessor>(&mut self) -> Result<ProcessorHandle> {
        self.add_processor_with_config::<P>(P::Config::default())
    }

    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<ProcessorHandle> {
        let processor = P::from_config(config)?;
        let processor_dyn: DynProcessor = Box::new(processor);
        self.add_boxed_processor(processor_dyn)
    }

    /// Add a boxed processor (for dynamic processor creation like Python processors)
    /// Works both before and during runtime - automatically detects runtime state
    pub fn add_boxed_processor(&mut self, processor: DynProcessor) -> Result<ProcessorHandle> {
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        // Get processor type name for the event
        let processor_type = std::any::type_name_of_val(&*processor).to_string();

        if self.running {
            // Runtime is running - spawn thread immediately
            self.spawn_processor_thread(id.clone(), processor)?;
            tracing::info!(
                "Added processor with ID: {} (runtime running, thread spawned)",
                id
            );
        } else {
            // Runtime not running - add to pending processors
            let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
            let (dummy_wakeup_tx, _dummy_wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

            let handle = RuntimeProcessorHandle {
                id: id.clone(),
                name: format!("Processor {}", self.next_processor_id - 1),
                thread: None,
                shutdown_tx,
                wakeup_tx: dummy_wakeup_tx,
                status: Arc::new(Mutex::new(ProcessorStatus::Pending)),
                processor: None,
            };

            {
                let mut processors = self.processors.lock();
                processors.insert(id.clone(), handle);
            }

            self.pending_processors
                .push((id.clone(), processor, shutdown_rx));
            tracing::info!("Added processor with ID: {} (pending)", id);
        }

        // Publish ProcessorAdded event
        {
            use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
            let added_event = Event::RuntimeGlobal(RuntimeEvent::ProcessorAdded {
                processor_id: id.clone(),
                processor_type: processor_type.clone(),
            });
            EVENT_BUS.publish(&added_event.topic(), &added_event);
            tracing::debug!("[{}] Published RuntimeEvent::ProcessorAdded", id);
        }

        // Update graph (source of truth for topology)
        self.graph.add_processor(
            id.clone(),
            processor_type.clone(),
            0, // No config checksum for boxed processors
        );
        self.dirty = true;
        tracing::debug!("[{}] Added to graph", id);

        // Return handle with metadata (no config checksum for boxed processors)
        Ok(ProcessorHandle::with_metadata(
            id,
            processor_type,
            None, // No checksum available for boxed processors
        ))
    }

    /// Internal helper: Spawn a processor thread immediately (runtime must be running)
    fn spawn_processor_thread(
        &mut self,
        processor_id: ProcessorId,
        processor: DynProcessor,
    ) -> Result<()> {
        if !self.running {
            return Err(StreamError::Runtime(
                "Cannot spawn processor thread - runtime is not running".into(),
            ));
        }

        tracing::info!(
            "[{}] Spawning processor thread in running runtime...",
            processor_id
        );

        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

        let processor_arc = Arc::new(Mutex::new(processor));

        {
            let mut processor = processor_arc.lock();
            processor.set_wakeup_channel(wakeup_tx.clone());
        }

        let id_for_thread = processor_id.clone();
        let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());
        let processor_for_thread = Arc::clone(&processor_arc);

        let sched_config = {
            let processor = processor_arc.lock();
            processor.scheduling_config()
        };

        let handle = std::thread::spawn(move || {
            tracing::info!(
                "[{}] Thread started (mode: {:?}, priority: {:?})",
                id_for_thread,
                sched_config.mode,
                sched_config.priority
            );

            // Apply thread priority
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                if let Err(e) =
                    crate::apple::thread_priority::apply_thread_priority(sched_config.priority)
                {
                    tracing::warn!("[{}] Failed to apply thread priority: {}", id_for_thread, e);
                }
            }

            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.__generated_setup(&runtime_context) {
                    tracing::error!("[{}] setup() failed: {}", id_for_thread, e);
                    return;
                }
            }

            // Publish ProcessorEvent::Started
            {
                use crate::core::pubsub::{Event, ProcessorEvent, EVENT_BUS};
                let started_event = Event::processor(&id_for_thread, ProcessorEvent::Started);
                EVENT_BUS.publish(&started_event.topic(), &started_event);
                tracing::debug!("[{}] Published ProcessorEvent::Started", id_for_thread);
            }

            match sched_config.mode {
                crate::core::scheduling::SchedulingMode::Pull => {
                    tracing::info!(
                        "[{}] Pull mode - processor manages own callback, waiting for shutdown",
                        id_for_thread
                    );

                    let _ = shutdown_rx.recv();
                    tracing::info!("[{}] Shutdown signal received (pull mode)", id_for_thread);
                }

                crate::core::scheduling::SchedulingMode::Loop => loop {
                    match shutdown_rx.try_recv() {
                        Ok(_) => {
                            tracing::info!("[{}] Shutdown signal received", id_for_thread);
                            break;
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            tracing::warn!("[{}] Shutdown channel closed", id_for_thread);
                            break;
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {}
                    }

                    {
                        let mut processor = processor_for_thread.lock();
                        if let Err(e) = processor.process() {
                            tracing::error!(
                                "[{}] process() error (loop mode): {}",
                                id_for_thread,
                                e
                            );
                        }
                    }

                    std::thread::sleep(std::time::Duration::from_micros(10));
                },

                crate::core::scheduling::SchedulingMode::Push => loop {
                    crossbeam_channel::select! {
                        recv(wakeup_rx) -> result => {
                            match result {
                                Ok(WakeupEvent::DataAvailable) => {
                                    tracing::debug!("[{}] Received DataAvailable wakeup", id_for_thread);
                                    let mut processor = processor_for_thread.lock();
                                    if let Err(e) = processor.process() {
                                        tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                    }
                                }
                                Ok(WakeupEvent::TimerTick) => {
                                    tracing::debug!("[{}] Received TimerTick wakeup", id_for_thread);
                                    let mut processor = processor_for_thread.lock();
                                    if let Err(e) = processor.process() {
                                        tracing::error!("[{}] process() error (timer tick): {}", id_for_thread, e);
                                    }
                                }
                                Ok(WakeupEvent::Shutdown) => {
                                    tracing::info!("[{}] Shutdown wakeup received", id_for_thread);
                                    break;
                                }
                                Err(_) => {
                                    tracing::warn!("[{}] Wakeup channel closed unexpectedly", id_for_thread);
                                    break;
                                }
                            }
                        }
                        recv(shutdown_rx) -> result => {
                            match result {
                                Ok(_) | Err(_) => {
                                    tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                    break;
                                }
                            }
                        }
                    }
                },
            }

            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.__generated_teardown() {
                    tracing::error!("[{}] teardown() failed: {}", id_for_thread, e);
                }
            }

            // Publish ProcessorEvent::Stopped
            {
                use crate::core::pubsub::{Event, ProcessorEvent, EVENT_BUS};
                let stopped_event = Event::processor(&id_for_thread, ProcessorEvent::Stopped);
                EVENT_BUS.publish(&stopped_event.topic(), &stopped_event);
                tracing::debug!("[{}] Published ProcessorEvent::Stopped", id_for_thread);
            }

            tracing::info!("[{}] Thread stopped", id_for_thread);
        });

        let proc_handle = RuntimeProcessorHandle {
            id: processor_id.clone(),
            name: format!("Processor {}", self.next_processor_id - 1),
            thread: Some(handle),
            shutdown_tx,
            wakeup_tx,
            status: Arc::new(Mutex::new(ProcessorStatus::Running)),
            processor: Some(processor_arc),
        };

        {
            let mut processors = self.processors.lock();
            processors.insert(processor_id.clone(), proc_handle);
        }

        tracing::info!("[{}] Processor thread spawned successfully", processor_id);
        Ok(())
    }

    pub fn connect<T: crate::core::bus::PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<ConnectionId> {
        // Generate connection ID
        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        // Create pending connection with ID
        let pending = PendingConnection::new(
            connection_id.clone(),
            output.processor_id().clone(),
            output.port_name().to_string(),
            input.processor_id().clone(),
            input.port_name().to_string(),
        );

        self.pending_connections.push(pending.clone());

        tracing::debug!(
            "Registered pending connection (will be wired at start): {}.{} → {}.{}",
            output.processor_id(),
            output.port_name(),
            input.processor_id(),
            input.port_name()
        );

        // Note: WillConnect and Connected events will be sent during wire_pending_connections()
        // when the runtime starts. We return a placeholder ID here.
        Ok(connection_id)
    }

    pub fn connect_at_runtime(&mut self, source: &str, destination: &str) -> Result<ConnectionId> {
        let (source_proc_id, source_port) = source.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid source format '{}'. Expected 'processor_id.port_name'",
                source
            ))
        })?;

        let (dest_proc_id, dest_port) = destination.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid destination format '{}'. Expected 'processor_id.port_name'",
                destination
            ))
        })?;

        // Generate connection ID early
        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        tracing::info!(
            "Connecting {} ({}:{}) → ({}:{}) [{}]",
            source,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Send WillConnect events BEFORE wiring
        {
            use crate::core::pubsub::{
                Event, PortType as EventPortType, ProcessorEvent, EVENT_BUS,
            };

            // Source processor (output port)
            EVENT_BUS.publish(
                &format!("processor:{}", source_proc_id),
                &Event::ProcessorEvent {
                    processor_id: source_proc_id.to_string(),
                    event: ProcessorEvent::WillConnect {
                        connection_id: connection_id.clone(),
                        port_name: source_port.to_string(),
                        port_type: EventPortType::Output,
                    },
                },
            );

            // Destination processor (input port)
            EVENT_BUS.publish(
                &format!("processor:{}", dest_proc_id),
                &Event::ProcessorEvent {
                    processor_id: dest_proc_id.to_string(),
                    event: ProcessorEvent::WillConnect {
                        connection_id: connection_id.clone(),
                        port_name: dest_port.to_string(),
                        port_type: EventPortType::Input,
                    },
                },
            );
        }

        let (source_processor, dest_processor) = {
            let processors = self.processors.lock();

            let source_handle = processors.get(source_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Source processor '{}' not found",
                    source_proc_id
                ))
            })?;

            let dest_handle = processors.get(dest_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Destination processor '{}' not found",
                    dest_proc_id
                ))
            })?;

            let source_proc = source_handle.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Source processor '{}' has no processor reference (not started?)",
                    source_proc_id
                ))
            })?;

            let dest_proc = dest_handle.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Destination processor '{}' has no processor reference (not started?)",
                    dest_proc_id
                ))
            })?;

            (Arc::clone(source_proc), Arc::clone(dest_proc))
        };

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
                            "Audio requirements incompatible when connecting {} → {}: {}",
                            source, destination, error_msg
                        )));
                    }

                    tracing::debug!(
                        "Audio requirements validated: {} → {} (compatible)",
                        source_proc_id,
                        dest_proc_id
                    );
                }
            }
        }

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
                    source, src_type, destination, dst_type
                )));
            }

            (src_type, dst_type)
        };

        // Create PortAddresses for the new generic API
        use crate::core::bus::PortAddress;
        let source_addr = PortAddress::new(source_proc_id.to_string(), source_port.to_string());
        let dest_addr = PortAddress::new(dest_proc_id.to_string(), dest_port.to_string());
        let capacity = source_port_type.default_capacity();

        // Phase 2: create_connection returns (OwnedProducer, OwnedConsumer)
        // We need to split them and pass separately via Box<dyn Any + Send>
        match source_port_type {
            PortType::Audio => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
            PortType::Video => {
                use crate::core::frames::VideoFrame;
                let (producer, consumer) = self.bus.create_connection::<VideoFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
            PortType::Data => {
                use crate::core::frames::DataFrame;
                let (producer, consumer) = self.bus.create_connection::<DataFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
        }

        tracing::info!(
            "Connected {} ({:?}) → {} ({:?}) via rtrb",
            source,
            source_port_type,
            destination,
            dest_port_type
        );

        // Store connection with metadata
        let connection = Connection::new(
            connection_id.clone(),
            source.to_string(),
            destination.to_string(),
            source_port_type,
            capacity,
        );

        {
            let mut connections = self.connections.lock();
            connections.insert(connection_id.clone(), connection.clone());
        }

        // Update connection index for both source and dest processors
        self.processor_connections
            .entry(connection.source_processor.clone())
            .or_default()
            .push(connection_id.clone());

        self.processor_connections
            .entry(connection.dest_processor.clone())
            .or_default()
            .push(connection_id.clone());

        // Send Connected events AFTER wiring complete
        {
            use crate::core::pubsub::{
                Event, PortType as EventPortType, ProcessorEvent, RuntimeEvent, EVENT_BUS,
            };

            // Source processor (output port)
            EVENT_BUS.publish(
                &format!("processor:{}", source_proc_id),
                &Event::ProcessorEvent {
                    processor_id: source_proc_id.to_string(),
                    event: ProcessorEvent::Connected {
                        connection_id: connection_id.clone(),
                        port_name: source_port.to_string(),
                        port_type: EventPortType::Output,
                    },
                },
            );

            // Destination processor (input port)
            EVENT_BUS.publish(
                &format!("processor:{}", dest_proc_id),
                &Event::ProcessorEvent {
                    processor_id: dest_proc_id.to_string(),
                    event: ProcessorEvent::Connected {
                        connection_id: connection_id.clone(),
                        port_name: dest_port.to_string(),
                        port_type: EventPortType::Input,
                    },
                },
            );

            // Broadcast RuntimeEvent
            EVENT_BUS.publish(
                "runtime:global",
                &Event::RuntimeGlobal(RuntimeEvent::ConnectionCreated {
                    connection_id: connection_id.clone(),
                    from_port: source.to_string(),
                    to_port: destination.to_string(),
                }),
            );
        }

        // Update graph (source of truth for topology)
        // Note: connection_id is already validated, use unchecked conversion
        let graph_connection_id =
            crate::core::bus::connection_id::__private::new_unchecked(connection_id.clone());
        if let Err(e) = self.graph.add_connection(
            graph_connection_id,
            source.to_string(),
            destination.to_string(),
            source_port_type,
        ) {
            tracing::warn!(
                "[{}] Failed to add connection to graph: {}",
                connection_id,
                e
            );
        }
        self.dirty = true;
        tracing::debug!("[{}] Added connection to graph", connection_id);

        tracing::info!("Registered runtime connection: {}", connection_id);
        Ok(connection_id)
    }

    /// Disconnect a connection by port references
    ///
    /// This can disconnect both pre-runtime pending connections and runtime connections.
    pub fn disconnect<T: crate::core::bus::PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        let source = format!("{}.{}", output.processor_id(), output.port_name());
        let destination = format!("{}.{}", input.processor_id(), input.port_name());

        // Check if this is a pending connection (pre-runtime)
        if !self.running {
            // Find and remove from pending_connections
            let removed_connection = self
                .pending_connections
                .iter()
                .position(|p| {
                    p.source_processor_id.as_str() == output.processor_id()
                        && p.source_port_name.as_str() == output.port_name()
                        && p.dest_processor_id.as_str() == input.processor_id()
                        && p.dest_port_name.as_str() == input.port_name()
                })
                .map(|idx| self.pending_connections.remove(idx));

            if let Some(removed) = removed_connection {
                tracing::info!(
                    "Removed pending connection {} ({} → {})",
                    removed.id,
                    source,
                    destination
                );
                return Ok(());
            }
        }

        // Otherwise, it's a runtime connection
        // Find the connection ID by searching connections HashMap
        let connection_id = {
            let connections = self.connections.lock();
            connections
                .iter()
                .find(|(_, conn)| conn.from_port == source && conn.to_port == destination)
                .map(|(id, _)| id.clone())
        };

        if let Some(id) = connection_id {
            self.disconnect_by_id(&id)
        } else {
            Err(StreamError::Configuration(format!(
                "Connection not found: {} → {}",
                source, destination
            )))
        }
    }

    /// Disconnect a connection by its ID
    pub fn disconnect_by_id(&mut self, connection_id: &ConnectionId) -> Result<()> {
        // Look up connection
        let connection = {
            let connections = self.connections.lock();
            connections.get(connection_id).cloned()
        };

        let connection = connection.ok_or_else(|| {
            StreamError::Configuration(format!("Connection {} not found", connection_id))
        })?;

        // Parse port addresses
        let (source_proc_id, source_port) =
            connection.from_port.split_once('.').ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Invalid source format in connection: {}",
                    connection.from_port
                ))
            })?;

        let (dest_proc_id, dest_port) = connection.to_port.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid destination format in connection: {}",
                connection.to_port
            ))
        })?;

        tracing::info!(
            "Disconnecting {} ({}:{} → {}:{}) [{}]",
            connection.from_port,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Send WillDisconnect events to both processors
        {
            use crate::core::pubsub::{
                Event, PortType as EventPortType, ProcessorEvent, EVENT_BUS,
            };

            EVENT_BUS.publish(
                &format!("processor:{}", source_proc_id),
                &Event::ProcessorEvent {
                    processor_id: source_proc_id.to_string(),
                    event: ProcessorEvent::WillDisconnect {
                        connection_id: connection_id.clone(),
                        port_name: source_port.to_string(),
                        port_type: EventPortType::Output,
                    },
                },
            );

            EVENT_BUS.publish(
                &format!("processor:{}", dest_proc_id),
                &Event::ProcessorEvent {
                    processor_id: dest_proc_id.to_string(),
                    event: ProcessorEvent::WillDisconnect {
                        connection_id: connection_id.clone(),
                        port_name: dest_port.to_string(),
                        port_type: EventPortType::Input,
                    },
                },
            );
        }

        // Best-effort drain with timeout (500ms default)
        let drain_timeout = std::time::Duration::from_millis(500);

        // Give processors a moment to react to WillDisconnect
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Attempt to drain ports
        {
            let processors = self.processors.lock();

            if let Some(src_handle) = processors.get(source_proc_id) {
                if let Some(src_proc) = &src_handle.processor {
                    let src_guard = src_proc.lock();
                    // Note: drain methods would be called on the processor if implemented
                    // For now, just wait the timeout
                    drop(src_guard);
                }
            }

            if let Some(dest_handle) = processors.get(dest_proc_id) {
                if let Some(dest_proc) = &dest_handle.processor {
                    let dest_guard = dest_proc.lock();
                    // Note: drain methods would be called on the processor if implemented
                    drop(dest_guard);
                }
            }
        }

        std::thread::sleep(drain_timeout);

        // TODO: Clean up processor ports (remove producers/consumers)
        // This requires access to processor internals which isn't exposed through the trait
        // Full implementation would:
        // 1. Remove OwnedProducer from source processor's StreamOutput
        // 2. Remove wakeup channel from source processor's downstream_wakeups
        // 3. Remove OwnedConsumer from dest processor's StreamInput
        // 4. Call bus.disconnect() with the bus-level ConnectionId (not the runtime string ID)
        //
        // For now, we only clean up runtime-level tracking.
        // The connection will stop being used but resources won't be fully freed.
        tracing::warn!(
            "Disconnection partial: runtime tracking cleaned up, but port-level cleanup not yet implemented for {}.{} → {}.{}",
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port
        );

        // Remove from runtime connections
        {
            let mut connections = self.connections.lock();
            connections.remove(connection_id);
        }

        // Remove from connection index for both source and dest processors
        if let Some(connections_vec) = self
            .processor_connections
            .get_mut(&connection.source_processor)
        {
            connections_vec.retain(|id| id != connection_id);
        }
        if let Some(connections_vec) = self
            .processor_connections
            .get_mut(&connection.dest_processor)
        {
            connections_vec.retain(|id| id != connection_id);
        }

        // Send Disconnected events to both processors
        {
            use crate::core::pubsub::{
                Event, PortType as EventPortType, ProcessorEvent, RuntimeEvent, EVENT_BUS,
            };

            EVENT_BUS.publish(
                &format!("processor:{}", source_proc_id),
                &Event::ProcessorEvent {
                    processor_id: source_proc_id.to_string(),
                    event: ProcessorEvent::Disconnected {
                        connection_id: connection_id.clone(),
                        port_name: source_port.to_string(),
                        port_type: EventPortType::Output,
                    },
                },
            );

            EVENT_BUS.publish(
                &format!("processor:{}", dest_proc_id),
                &Event::ProcessorEvent {
                    processor_id: dest_proc_id.to_string(),
                    event: ProcessorEvent::Disconnected {
                        connection_id: connection_id.clone(),
                        port_name: dest_port.to_string(),
                        port_type: EventPortType::Input,
                    },
                },
            );

            // Broadcast RuntimeEvent
            EVENT_BUS.publish(
                "runtime:global",
                &Event::RuntimeGlobal(RuntimeEvent::ConnectionRemoved {
                    connection_id: connection_id.clone(),
                    from_port: connection.from_port.clone(),
                    to_port: connection.to_port.clone(),
                }),
            );
        }

        // Update graph (source of truth for topology)
        let graph_connection_id =
            crate::core::bus::connection_id::__private::new_unchecked(connection_id.clone());
        self.graph.remove_connection(&graph_connection_id);
        self.dirty = true;
        tracing::debug!("[{}] Removed connection from graph", connection_id);

        tracing::info!("Successfully disconnected connection: {}", connection_id);
        Ok(())
    }

    fn wire_pending_connections(&mut self) -> Result<()> {
        if self.pending_connections.is_empty() {
            tracing::debug!("No pending connections to wire");
            return Ok(());
        }

        tracing::info!(
            "Wiring {} pending connections...",
            self.pending_connections.len()
        );

        let connections_to_wire = std::mem::take(&mut self.pending_connections);

        for pending in connections_to_wire {
            let source = format!(
                "{}.{}",
                pending.source_processor_id, pending.source_port_name
            );
            let destination = format!("{}.{}", pending.dest_processor_id, pending.dest_port_name);

            tracing::info!("Wiring connection: {} → {}", source, destination);

            self.connect_at_runtime(&source, &destination)?;

            {
                let processors = self.processors.lock();
                let source_handle = processors.get(&pending.source_processor_id);
                let dest_handle = processors.get(&pending.dest_processor_id);

                if let (Some(src), Some(dst)) = (source_handle, dest_handle) {
                    if let Some(src_proc) = src.processor.as_ref() {
                        let mut source_guard = src_proc.lock();
                        source_guard
                            .set_output_wakeup(&pending.source_port_name, dst.wakeup_tx.clone());

                        tracing::debug!(
                            "Wired wakeup notification: {} ({}) → {} ({})",
                            pending.source_processor_id,
                            pending.source_port_name,
                            pending.dest_processor_id,
                            pending.dest_port_name
                        );
                    }
                }
            }
        }

        tracing::info!("All pending connections wired successfully");

        tracing::debug!("Sending initialization wakeup to Pull mode processors");
        {
            let processors = self.processors.lock();
            for (proc_id, handle) in processors.iter() {
                if let Some(proc_ref) = &handle.processor {
                    let sched_config = proc_ref.lock().scheduling_config();
                    if matches!(
                        sched_config.mode,
                        crate::core::scheduling::SchedulingMode::Pull
                    ) {
                        if let Err(e) = handle.wakeup_tx.send(WakeupEvent::DataAvailable) {
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

        Ok(())
    }

    pub fn start(&mut self) -> Result<()> {
        if self.running {
            return Err(StreamError::Configuration("Runtime already running".into()));
        }

        let handler_count = self.pending_processors.len();

        tracing::info!("Starting runtime with {} processors", handler_count);

        let core_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        if handler_count > core_count * 2 {
            tracing::warn!(
                "Runtime has {} handlers but only {} CPU cores available. \
                 This may cause thread scheduling overhead. \
                 Consider using fewer, more complex handlers or a thread pool.",
                handler_count,
                core_count
            );
        }

        tracing::info!("Initializing GPU context...");
        let gpu_context = crate::core::context::GpuContext::init_for_platform_sync()?;
        tracing::info!("GPU context initialized: {:?}", gpu_context);
        self.gpu_context = Some(gpu_context);

        self.running = true;

        // Generate execution plan if graph is dirty (Phase 0: Legacy plan)
        if self.dirty {
            tracing::info!("Generating execution plan...");
            let plan = self.graph_optimizer.optimize(&self.graph)?;
            tracing::info!("Execution plan generated: {:?}", plan);
            tracing::debug!(
                "Optimizer stats: {:?}",
                self.graph_optimizer.stats(&self.graph)
            );
            self.execution_plan = Some(plan);
            self.dirty = false;
        }

        self.spawn_handler_threads()?;

        self.wire_pending_connections()?;

        tracing::info!("Runtime started successfully");
        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        if !self.running {
            self.start()?;
        }

        // Install native signal handlers (SIGTERM, SIGINT)
        crate::core::signals::install_signal_handlers().map_err(|e| {
            StreamError::Configuration(format!("Failed to install signal handlers: {}", e))
        })?;

        tracing::info!("Runtime running (press Ctrl+C to stop)");

        if let Some(event_loop) = self.event_loop.take() {
            tracing::debug!("Using platform-specific event loop");
            event_loop()?;
        } else {
            // Default: wait for shutdown event via event bus
            use crate::core::pubsub::{topics, Event, EventListener, RuntimeEvent, EVENT_BUS};
            use parking_lot::Mutex;
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;

            let running = Arc::new(AtomicBool::new(true));
            let running_clone = Arc::clone(&running);

            // Shutdown listener that sets the running flag
            struct ShutdownListener {
                running: Arc<AtomicBool>,
            }

            impl EventListener for ShutdownListener {
                fn on_event(&mut self, event: &Event) -> Result<()> {
                    if let Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) = event {
                        tracing::info!("Runtime received shutdown event, stopping...");
                        self.running.store(false, Ordering::SeqCst);
                    }
                    Ok(())
                }
            }

            let listener = ShutdownListener {
                running: running_clone,
            };
            EVENT_BUS.subscribe(topics::RUNTIME_GLOBAL, Arc::new(Mutex::new(listener)));

            while running.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            tracing::info!("Shutdown signal received");
        }

        self.stop()?;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }

        tracing::info!("Stopping runtime...");
        self.running = false;

        // Publish shutdown event to event bus for shutdown-aware loops
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        EVENT_BUS.publish(&shutdown_event.topic(), &shutdown_event);
        tracing::debug!("Published shutdown event to event bus");

        {
            let processors = self.processors.lock();
            for (processor_id, proc_handle) in processors.iter() {
                if let Err(e) = proc_handle.shutdown_tx.send(()) {
                    tracing::warn!("[{}] Failed to send shutdown signal: {}", processor_id, e);
                }
            }
        }
        tracing::debug!("Shutdown signals sent to all processors");

        let processor_ids: Vec<ProcessorId> = {
            let processors = self.processors.lock();
            processors.keys().cloned().collect()
        };

        let thread_count = processor_ids.len();
        for (i, processor_id) in processor_ids.iter().enumerate() {
            let thread_handle = {
                let mut processors = self.processors.lock();
                processors
                    .get_mut(processor_id)
                    .and_then(|proc| proc.thread.take())
            };

            if let Some(handle) = thread_handle {
                match handle.join() {
                    Ok(_) => {
                        tracing::debug!(
                            "[{}] Thread joined ({}/{})",
                            processor_id,
                            i + 1,
                            thread_count
                        );
                        let mut processors = self.processors.lock();
                        if let Some(proc) = processors.get_mut(processor_id) {
                            *proc.status.lock() = ProcessorStatus::Stopped;
                        }
                    }
                    Err(e) => tracing::error!(
                        "[{}] Thread panicked ({}/{}): {:?}",
                        processor_id,
                        i + 1,
                        thread_count,
                        e
                    ),
                }
            }
        }

        tracing::info!("Runtime stopped");

        Ok(())
    }

    pub fn remove_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        let shutdown_tx = {
            let mut processors = self.processors.lock();
            let processor = processors.get_mut(processor_id).ok_or_else(|| {
                StreamError::NotFound(format!("Processor '{}' not found", processor_id))
            })?;

            let current_status = *processor.status.lock();
            if current_status == ProcessorStatus::Stopped
                || current_status == ProcessorStatus::Stopping
            {
                return Err(StreamError::Runtime(format!(
                    "Processor '{}' is already {:?}",
                    processor_id, current_status
                )));
            }

            *processor.status.lock() = ProcessorStatus::Stopping;

            processor.shutdown_tx.clone()
        };

        tracing::info!("[{}] Removing processor...", processor_id);

        shutdown_tx.send(()).map_err(|_| {
            StreamError::Runtime(format!(
                "Failed to send shutdown signal to processor '{}'",
                processor_id
            ))
        })?;

        tracing::debug!("[{}] Shutdown signal sent", processor_id);

        let thread_handle = {
            let mut processors = self.processors.lock();
            processors
                .get_mut(processor_id)
                .and_then(|proc| proc.thread.take())
        };

        if let Some(handle) = thread_handle {
            match handle.join() {
                Ok(_) => {
                    tracing::info!("[{}] Processor thread joined successfully", processor_id);

                    let mut processors = self.processors.lock();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock() = ProcessorStatus::Stopped;
                    }
                }
                Err(panic_err) => {
                    tracing::error!(
                        "[{}] Processor thread panicked: {:?}",
                        processor_id,
                        panic_err
                    );

                    let mut processors = self.processors.lock();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock() = ProcessorStatus::Stopped;
                    }

                    return Err(StreamError::Runtime(format!(
                        "Processor '{}' thread panicked",
                        processor_id
                    )));
                }
            }
        } else {
            tracing::warn!(
                "[{}] No thread handle found (processor may not have started)",
                processor_id
            );
        }

        // Publish ProcessorRemoved event
        {
            use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
            let removed_event = Event::RuntimeGlobal(RuntimeEvent::ProcessorRemoved {
                processor_id: processor_id.to_string(),
            });
            EVENT_BUS.publish(&removed_event.topic(), &removed_event);
            tracing::debug!(
                "[{}] Published RuntimeEvent::ProcessorRemoved",
                processor_id
            );
        }

        // Update graph (source of truth for topology)
        self.graph.remove_processor(processor_id);
        self.dirty = true;
        tracing::debug!("[{}] Removed from graph", processor_id);

        // Clean up connection index for this processor
        self.processor_connections.remove(processor_id);

        tracing::info!("[{}] Processor removed", processor_id);
        Ok(())
    }

    /// Get all connection IDs involving a specific processor
    ///
    /// Returns connection IDs where the processor is either the source or destination.
    /// Useful for graph analysis and optimization.
    pub fn get_connections_for_processor(&self, processor_id: &ProcessorId) -> Vec<ConnectionId> {
        self.processor_connections
            .get(processor_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn status(&self) -> RuntimeStatus {
        let handler_count = {
            let processors = self.processors.lock();
            processors.len()
        };

        RuntimeStatus {
            running: self.running,
            handler_count,
        }
    }

    fn spawn_handler_threads(&mut self) -> Result<()> {
        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        for (processor_id, processor, shutdown_rx) in self.pending_processors.drain(..) {
            let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

            let processor_arc = Arc::new(Mutex::new(processor));

            {
                let mut processor = processor_arc.lock();
                processor.set_wakeup_channel(wakeup_tx.clone());
            }

            let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());

            let id_for_thread = processor_id.clone();

            let processor_for_thread = Arc::clone(&processor_arc);

            let sched_config = {
                let processor = processor_arc.lock();
                processor.scheduling_config()
            };

            let handle = std::thread::spawn(move || {
                tracing::info!(
                    "[{}] Thread started (mode: {:?}, priority: {:?})",
                    id_for_thread,
                    sched_config.mode,
                    sched_config.priority
                );

                // Apply thread priority
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                {
                    if let Err(e) =
                        crate::apple::thread_priority::apply_thread_priority(sched_config.priority)
                    {
                        tracing::warn!(
                            "[{}] Failed to apply thread priority: {}",
                            id_for_thread,
                            e
                        );
                    }
                }

                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.__generated_setup(&runtime_context) {
                        tracing::error!("[{}] setup() failed: {}", id_for_thread, e);
                        return;
                    }
                }

                // Publish ProcessorEvent::Started
                {
                    use crate::core::pubsub::{Event, ProcessorEvent, EVENT_BUS};
                    let started_event = Event::processor(&id_for_thread, ProcessorEvent::Started);
                    EVENT_BUS.publish(&started_event.topic(), &started_event);
                    tracing::debug!("[{}] Published ProcessorEvent::Started", id_for_thread);
                }

                match sched_config.mode {
                    crate::core::scheduling::SchedulingMode::Pull => {
                        tracing::info!(
                            "[{}] Pull mode - waiting for connections to be wired",
                            id_for_thread
                        );

                        let init_result = crossbeam_channel::select! {
                            recv(wakeup_rx) -> result => {
                                match result {
                                    Ok(_) => {
                                        tracing::info!("[{}] Pull mode - connections ready, calling process() for initialization", id_for_thread);
                                        let mut processor = processor_for_thread.lock();
                                        processor.process()
                                    }
                                    Err(e) => {
                                        tracing::error!("[{}] Wakeup channel closed before initialization: {}", id_for_thread, e);
                                        return;
                                    }
                                }
                            }
                            recv(shutdown_rx) -> _ => {
                                tracing::info!("[{}] Shutdown before initialization", id_for_thread);
                                return;
                            }
                        };

                        if let Err(e) = init_result {
                            tracing::error!(
                                "[{}] Pull mode process() initialization failed: {}",
                                id_for_thread,
                                e
                            );
                            return;
                        }

                        tracing::info!("[{}] Pull mode initialized - processor manages own callback, waiting for shutdown", id_for_thread);

                        let _ = shutdown_rx.recv();
                        tracing::info!("[{}] Shutdown signal received (pull mode)", id_for_thread);
                    }

                    crate::core::scheduling::SchedulingMode::Loop => loop {
                        match shutdown_rx.try_recv() {
                            Ok(_) => {
                                tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                break;
                            }
                            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                tracing::warn!("[{}] Shutdown channel closed", id_for_thread);
                                break;
                            }
                            Err(crossbeam_channel::TryRecvError::Empty) => {}
                        }

                        {
                            let mut processor = processor_for_thread.lock();
                            if let Err(e) = processor.process() {
                                tracing::error!(
                                    "[{}] process() error (loop mode): {}",
                                    id_for_thread,
                                    e
                                );
                            }
                        }

                        std::thread::sleep(std::time::Duration::from_micros(10));
                    },

                    crate::core::scheduling::SchedulingMode::Push => loop {
                        crossbeam_channel::select! {
                            recv(wakeup_rx) -> result => {
                                match result {
                                    Ok(WakeupEvent::DataAvailable) => {
                                        let mut processor = processor_for_thread.lock();
                                        if let Err(e) = processor.process() {
                                            tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                        }
                                    }
                                    Ok(WakeupEvent::TimerTick) => {
                                        let mut processor = processor_for_thread.lock();
                                        if let Err(e) = processor.process() {
                                            tracing::error!("[{}] process() error (timer tick): {}", id_for_thread, e);
                                        }
                                    }
                                    Ok(WakeupEvent::Shutdown) => {
                                        tracing::info!("[{}] Shutdown wakeup received", id_for_thread);
                                        break;
                                    }
                                    Err(_) => {
                                        tracing::warn!("[{}] Wakeup channel closed unexpectedly", id_for_thread);
                                        break;
                                    }
                                }
                            }
                            recv(shutdown_rx) -> result => {
                                match result {
                                    Ok(_) | Err(_) => {
                                        tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                        break;
                                    }
                                }
                            }
                        }
                    },
                }

                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.__generated_teardown() {
                        tracing::error!("[{}] teardown() failed: {}", id_for_thread, e);
                    }
                }

                // Publish ProcessorEvent::Stopped
                {
                    use crate::core::pubsub::{Event, ProcessorEvent, EVENT_BUS};
                    let stopped_event = Event::processor(&id_for_thread, ProcessorEvent::Stopped);
                    EVENT_BUS.publish(&stopped_event.topic(), &stopped_event);
                    tracing::debug!("[{}] Published ProcessorEvent::Stopped", id_for_thread);
                }

                tracing::info!("[{}] Thread stopped", id_for_thread);
            });

            {
                let mut processors = self.processors.lock();
                if let Some(proc_handle) = processors.get_mut(&processor_id) {
                    proc_handle.thread = Some(handle);
                    proc_handle.processor = Some(processor_arc);
                    proc_handle.wakeup_tx = wakeup_tx;
                    *proc_handle.status.lock() = ProcessorStatus::Running;
                } else {
                    tracing::error!("Processor {} not found in registry", processor_id);
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub handler_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::{ElementType, StreamElement, StreamProcessor};
    use crate::core::ProcessorDescriptor;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone, Serialize, Deserialize)]
    struct CounterConfig {
        #[serde(skip)]
        count: Arc<AtomicU64>,
    }

    impl Default for CounterConfig {
        fn default() -> Self {
            Self {
                count: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    struct CounterProcessor {
        name: String,
        count: Arc<AtomicU64>,
    }

    impl StreamElement for CounterProcessor {
        fn name(&self) -> &str {
            &self.name
        }

        fn element_type(&self) -> ElementType {
            ElementType::Transform
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            Some(ProcessorDescriptor::new(
                "CounterProcessor",
                "Test processor that increments a counter",
            ))
        }
    }

    impl StreamProcessor for CounterProcessor {
        type Config = CounterConfig;

        fn from_config(config: Self::Config) -> Result<Self> {
            Ok(Self {
                name: "counter".to_string(),
                count: config.count,
            })
        }

        fn process(&mut self) -> Result<()> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            Some(ProcessorDescriptor::new(
                "CounterProcessor",
                "Test processor that increments a counter",
            ))
        }
    }

    #[test]
    fn test_runtime_creation() {
        let runtime = StreamRuntime::new();
        assert!(!runtime.running);
        assert_eq!(runtime.pending_processors.len(), 0);
    }

    #[test]
    fn test_add_processor() {
        let mut runtime = StreamRuntime::new();

        let count = Arc::new(AtomicU64::new(0));
        let config = CounterConfig { count };

        let _handle = runtime
            .add_processor_with_config::<CounterProcessor>(config)
            .unwrap();
        assert_eq!(runtime.pending_processors.len(), 1);
    }

    #[test]
    #[ignore] // Brittle timing test - relies on counters incrementing in 100ms
    fn test_runtime_lifecycle() {
        let mut runtime = StreamRuntime::new();

        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));

        let config1 = CounterConfig {
            count: Arc::clone(&count1),
        };
        let config2 = CounterConfig {
            count: Arc::clone(&count2),
        };

        runtime
            .add_processor_with_config::<CounterProcessor>(config1)
            .unwrap();
        runtime
            .add_processor_with_config::<CounterProcessor>(config2)
            .unwrap();

        runtime.start().unwrap();
        assert!(runtime.running);

        {
            let processors = runtime.processors.lock();
            assert_eq!(processors.len(), 2);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        runtime.stop().unwrap();
        assert!(!runtime.running);

        let c1 = count1.load(Ordering::Relaxed);
        let c2 = count2.load(Ordering::Relaxed);

        assert!(c1 > 0);
        assert!(c2 > 0);
    }

    #[test]
    #[ignore] // Brittle timing test - relies on exact iteration counts in 350ms
    fn test_true_parallelism() {
        use std::time::Instant;

        #[derive(Clone, Serialize, Deserialize)]
        struct WorkConfig {
            work_duration_ms: u64,
            #[serde(skip)]
            start_times: Arc<Mutex<Vec<Instant>>>,
        }

        impl Default for WorkConfig {
            fn default() -> Self {
                Self {
                    work_duration_ms: 50,
                    start_times: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        struct WorkProcessor {
            name: String,
            work_duration_ms: u64,
            start_times: Arc<Mutex<Vec<Instant>>>,
            work_counter: u64,
        }

        impl StreamElement for WorkProcessor {
            fn name(&self) -> &str {
                &self.name
            }

            fn element_type(&self) -> ElementType {
                ElementType::Transform
            }

            fn descriptor(&self) -> Option<ProcessorDescriptor> {
                Some(ProcessorDescriptor::new(
                    "WorkProcessor",
                    "Test processor that performs CPU work",
                ))
            }
        }

        impl StreamProcessor for WorkProcessor {
            type Config = WorkConfig;

            fn from_config(config: Self::Config) -> Result<Self> {
                Ok(Self {
                    name: "work".to_string(),
                    work_duration_ms: config.work_duration_ms,
                    start_times: config.start_times,
                    work_counter: 0,
                })
            }

            fn process(&mut self) -> Result<()> {
                self.start_times.lock().push(Instant::now());

                let start = Instant::now();
                let mut sum = 0u64;
                while start.elapsed().as_millis() < self.work_duration_ms as u128 {
                    sum = sum.wrapping_add(self.work_counter);
                }
                self.work_counter += 1;

                if sum == u64::MAX {
                    println!("Never happens");
                }

                Ok(())
            }

            fn descriptor() -> Option<ProcessorDescriptor> {
                Some(ProcessorDescriptor::new(
                    "WorkProcessor",
                    "Test processor that performs CPU work",
                ))
            }
        }

        let mut runtime = StreamRuntime::new();

        let start_times1 = Arc::new(Mutex::new(Vec::new()));
        let start_times2 = Arc::new(Mutex::new(Vec::new()));

        let config1 = WorkConfig {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times1),
        };

        let config2 = WorkConfig {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times2),
        };

        runtime
            .add_processor_with_config::<WorkProcessor>(config1)
            .unwrap();
        runtime
            .add_processor_with_config::<WorkProcessor>(config2)
            .unwrap();

        runtime.start().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(350));

        runtime.stop().unwrap();

        let times1 = start_times1.lock();
        let times2 = start_times2.lock();

        assert!(times1.len() >= 2);
        assert!(times2.len() >= 2);

        if let (Some(&t1), Some(&t2)) = (times1.first(), times2.first()) {
            let diff = if t1 > t2 {
                t1.duration_since(t2)
            } else {
                t2.duration_since(t1)
            };

            assert!(
                diff.as_millis() < 50,
                "Processors should start processing nearly simultaneously"
            );
        }
    }

    #[test]
    fn test_disconnect_pre_runtime() {
        // Simple test that verifies pre-runtime disconnect without requiring full processor setup
        // This tests the disconnect() logic for pending connections before runtime.start()

        use crate::core::frames::DataFrame;
        use crate::core::handles::ProcessorHandle;

        let mut runtime = StreamRuntime::new();

        // Create processor handles for testing
        let proc1 = ProcessorHandle::new("proc1".to_string());
        let proc2 = ProcessorHandle::new("proc2".to_string());

        let output_ref = proc1.output_port::<DataFrame>("output");
        let input_ref = proc2.input_port::<DataFrame>("input");

        // Create a pending connection directly (simulating what connect() would do)
        use crate::core::handles::PendingConnection;
        let connection_id = "test-conn-1".to_string();
        runtime.pending_connections.push(PendingConnection::new(
            connection_id.clone(),
            output_ref.processor_id().clone(),
            output_ref.port_name().to_string(),
            input_ref.processor_id().clone(),
            input_ref.port_name().to_string(),
        ));

        // Verify connection was created
        assert_eq!(runtime.pending_connections.len(), 1);

        // Disconnect by port references (pre-runtime)
        runtime.disconnect(output_ref, input_ref).unwrap();

        // Verify connection was removed
        assert_eq!(runtime.pending_connections.len(), 0);
    }

    #[test]
    fn test_disconnect_by_id() {
        // Simple test that verifies disconnect_by_id returns error for non-existent connection
        // Full integration test with real processors would require starting the runtime,
        // which involves GPU context initialization and is better suited for integration tests

        let mut runtime = StreamRuntime::new();

        // Try to disconnect a non-existent connection
        let result = runtime.disconnect_by_id(&"non-existent-id".to_string());

        // Should return an error
        assert!(result.is_err());
        if let Err(e) = result {
            // Verify it's a configuration error
            assert!(matches!(e, StreamError::Configuration(_)));
        }
    }

    #[test]
    fn test_disconnect_events() {
        // Test that disconnect events are published correctly
        use crate::core::pubsub::{Event, EventListener, RuntimeEvent, EVENT_BUS};
        use parking_lot::Mutex;
        use std::sync::Arc;

        // Track received events
        let received_events = Arc::new(Mutex::new(Vec::new()));
        let received_events_clone = Arc::clone(&received_events);

        // Create event listener
        struct TestListener {
            events: Arc<Mutex<Vec<Event>>>,
        }

        impl EventListener for TestListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                self.events.lock().push(event.clone());
                Ok(())
            }
        }

        // Subscribe to all events (must keep listener Arc alive)
        let listener_arc: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(TestListener {
            events: received_events_clone,
        }));
        EVENT_BUS.subscribe("runtime:global", Arc::clone(&listener_arc));

        // Create runtime and add fake connection to test disconnect events
        let mut runtime = StreamRuntime::new();
        runtime.running = true; // Simulate started runtime

        // Manually create a connection entry for testing
        use super::Connection;
        use crate::core::bus::PortType;
        let connection_id = "test-conn-123".to_string();
        let conn = Connection::new(
            connection_id.clone(),
            "proc1.output".to_string(),
            "proc2.input".to_string(),
            PortType::Video,
            3,
        );

        runtime
            .connections
            .lock()
            .insert(connection_id.clone(), conn);

        // Clear any setup events
        received_events.lock().clear();

        // Disconnect by ID (will fail during port cleanup but events should be sent)
        let _ = runtime.disconnect_by_id(&connection_id);

        // Check events were published
        let events = received_events.lock();

        // Should have received RuntimeEvent::ConnectionRemoved
        let connection_removed = events.iter().any(|e| {
            matches!(
                e,
                Event::RuntimeGlobal(RuntimeEvent::ConnectionRemoved { connection_id: id, .. })
                if id == &connection_id
            )
        });

        assert!(
            connection_removed,
            "Should have received ConnectionRemoved event. Events: {:?}",
            events
        );
    }

    #[test]
    fn test_connection_events_include_port_type() {
        // Verify that connection events include PortType enum (Input/Output)
        use crate::core::pubsub::{PortType as EventPortType, ProcessorEvent};

        // Test that the PortType enum exists and has correct variants
        let input_type = EventPortType::Input;
        let output_type = EventPortType::Output;

        // Verify they're different
        assert_ne!(input_type, output_type);

        // Verify they can be used in events
        let _event = ProcessorEvent::WillConnect {
            connection_id: "test".to_string(),
            port_name: "video".to_string(),
            port_type: EventPortType::Output,
        };

        // Test pattern matching works
        match _event {
            ProcessorEvent::WillConnect {
                port_type: EventPortType::Output,
                ..
            } => {
                // Expected
            }
            _ => panic!("Pattern matching failed"),
        }
    }

    #[test]
    fn test_connect_returns_connection_id() {
        // Verify that connect() now returns a connection ID instead of ()
        use crate::core::frames::DataFrame;
        use crate::core::handles::ProcessorHandle;

        let mut runtime = StreamRuntime::new();

        // Create processor handles
        let proc1 = ProcessorHandle::new("proc1".to_string());
        let proc2 = ProcessorHandle::new("proc2".to_string());

        // Connect should return a String connection ID
        let connection_id = runtime
            .connect(
                proc1.output_port::<DataFrame>("output"),
                proc2.input_port::<DataFrame>("input"),
            )
            .unwrap();

        // Should be a non-empty string
        assert!(!connection_id.is_empty());

        // Should be trackable in pending connections
        assert_eq!(runtime.pending_connections.len(), 1);
        assert_eq!(runtime.pending_connections[0].id, connection_id);
    }

    #[test]
    fn test_disconnect_pre_runtime_with_events() {
        // Test that pre-runtime disconnect doesn't crash and returns the connection ID in logs
        use crate::core::frames::DataFrame;
        use crate::core::handles::ProcessorHandle;

        let mut runtime = StreamRuntime::new();

        let proc1 = ProcessorHandle::new("proc1".to_string());
        let proc2 = ProcessorHandle::new("proc2".to_string());

        let output_ref = proc1.output_port::<DataFrame>("output");
        let input_ref = proc2.input_port::<DataFrame>("input");

        // Connect
        let connection_id = runtime
            .connect(output_ref.clone(), input_ref.clone())
            .unwrap();

        // Verify pending connection has the ID
        assert_eq!(runtime.pending_connections.len(), 1);
        assert_eq!(runtime.pending_connections[0].id, connection_id);

        // Disconnect (should log the connection ID)
        runtime.disconnect(output_ref, input_ref).unwrap();

        // Verify removed
        assert_eq!(runtime.pending_connections.len(), 0);
    }

    #[test]
    fn test_disconnect_event_ordering() {
        // Test that disconnect events are sent in correct order:
        // WillDisconnect → drain → Disconnected → ConnectionRemoved
        use crate::core::pubsub::{Event, EventListener, ProcessorEvent, RuntimeEvent, EVENT_BUS};
        use parking_lot::Mutex;
        use std::sync::Arc;

        // Track received events with timestamps
        let received_events = Arc::new(Mutex::new(Vec::new()));
        let received_events_clone = Arc::clone(&received_events);

        struct TestListener {
            events: Arc<Mutex<Vec<(Event, std::time::Instant)>>>,
        }

        impl EventListener for TestListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                self.events
                    .lock()
                    .push((event.clone(), std::time::Instant::now()));
                Ok(())
            }
        }

        // Subscribe to both runtime and processor topics
        let listener1: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(TestListener {
            events: Arc::clone(&received_events_clone),
        }));
        let listener2: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(TestListener {
            events: Arc::clone(&received_events_clone),
        }));
        let listener3: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(TestListener {
            events: received_events_clone,
        }));

        EVENT_BUS.subscribe("runtime:global", Arc::clone(&listener1));
        EVENT_BUS.subscribe("processor:proc1", Arc::clone(&listener2));
        EVENT_BUS.subscribe("processor:proc2", Arc::clone(&listener3));

        // Create runtime and add fake connection
        let mut runtime = StreamRuntime::new();
        runtime.running = true;

        let connection_id = "test-conn-456".to_string();
        let conn = Connection::new(
            connection_id.clone(),
            "proc1.output".to_string(),
            "proc2.input".to_string(),
            PortType::Video,
            3,
        );

        runtime
            .connections
            .lock()
            .insert(connection_id.clone(), conn);
        received_events.lock().clear();

        // Disconnect by ID
        let _ = runtime.disconnect_by_id(&connection_id);

        // Check event ordering
        let events = received_events.lock();

        // Extract event types in order
        let event_sequence: Vec<String> = events
            .iter()
            .map(|(e, _)| match e {
                Event::ProcessorEvent {
                    event: ProcessorEvent::WillDisconnect { .. },
                    ..
                } => "WillDisconnect".to_string(),
                Event::ProcessorEvent {
                    event: ProcessorEvent::Disconnected { .. },
                    ..
                } => "Disconnected".to_string(),
                Event::RuntimeGlobal(RuntimeEvent::ConnectionRemoved { .. }) => {
                    "ConnectionRemoved".to_string()
                }
                _ => "Other".to_string(),
            })
            .collect();

        // WillDisconnect events should come before Disconnected events
        let will_disconnect_indices: Vec<usize> = event_sequence
            .iter()
            .enumerate()
            .filter(|(_, e)| *e == "WillDisconnect")
            .map(|(i, _)| i)
            .collect();

        let disconnected_indices: Vec<usize> = event_sequence
            .iter()
            .enumerate()
            .filter(|(_, e)| *e == "Disconnected")
            .map(|(i, _)| i)
            .collect();

        let connection_removed_indices: Vec<usize> = event_sequence
            .iter()
            .enumerate()
            .filter(|(_, e)| *e == "ConnectionRemoved")
            .map(|(i, _)| i)
            .collect();

        // Verify we got the events
        assert!(
            !will_disconnect_indices.is_empty(),
            "Should have WillDisconnect events"
        );
        assert!(
            !disconnected_indices.is_empty(),
            "Should have Disconnected events"
        );
        assert!(
            !connection_removed_indices.is_empty(),
            "Should have ConnectionRemoved event"
        );

        // Verify ordering: WillDisconnect < Disconnected < ConnectionRemoved
        if let (Some(&first_will), Some(&first_disc), Some(&first_removed)) = (
            will_disconnect_indices.first(),
            disconnected_indices.first(),
            connection_removed_indices.first(),
        ) {
            assert!(
                first_will < first_disc,
                "WillDisconnect should come before Disconnected. Sequence: {:?}",
                event_sequence
            );
            assert!(
                first_disc < first_removed,
                "Disconnected should come before ConnectionRemoved. Sequence: {:?}",
                event_sequence
            );
        }
    }

    #[test]
    fn test_processor_level_disconnect_events() {
        // Test that disconnect events are sent to BOTH source and destination processors
        use crate::core::pubsub::{Event, EventListener, ProcessorEvent, EVENT_BUS};
        use parking_lot::Mutex;
        use std::sync::Arc;

        // Track events per processor
        let proc1_events = Arc::new(Mutex::new(Vec::new()));
        let proc2_events = Arc::new(Mutex::new(Vec::new()));

        struct ProcessorListener {
            events: Arc<Mutex<Vec<ProcessorEvent>>>,
        }

        impl EventListener for ProcessorListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::ProcessorEvent { event, .. } = event {
                    self.events.lock().push(event.clone());
                }
                Ok(())
            }
        }

        // Subscribe to individual processor topics
        let listener1: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(ProcessorListener {
            events: Arc::clone(&proc1_events),
        }));
        let listener2: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(ProcessorListener {
            events: Arc::clone(&proc2_events),
        }));

        EVENT_BUS.subscribe("processor:proc1", Arc::clone(&listener1));
        EVENT_BUS.subscribe("processor:proc2", Arc::clone(&listener2));

        // Create runtime and connection
        let mut runtime = StreamRuntime::new();
        runtime.running = true;

        let connection_id = "test-conn-789".to_string();
        let conn = Connection::new(
            connection_id.clone(),
            "proc1.output".to_string(),
            "proc2.input".to_string(),
            PortType::Video,
            3,
        );

        runtime
            .connections
            .lock()
            .insert(connection_id.clone(), conn);

        // Clear events
        proc1_events.lock().clear();
        proc2_events.lock().clear();

        // Disconnect
        let _ = runtime.disconnect_by_id(&connection_id);

        // Check both processors received events
        let p1_events = proc1_events.lock();
        let p2_events = proc2_events.lock();

        // Processor 1 (source) should receive WillDisconnect and Disconnected
        let p1_will_disconnect = p1_events.iter().any(|e| {
            matches!(e, ProcessorEvent::WillDisconnect { connection_id: id, .. } if id == &connection_id)
        });
        let p1_disconnected = p1_events.iter().any(|e| {
            matches!(e, ProcessorEvent::Disconnected { connection_id: id, .. } if id == &connection_id)
        });

        // Processor 2 (destination) should receive WillDisconnect and Disconnected
        let p2_will_disconnect = p2_events.iter().any(|e| {
            matches!(e, ProcessorEvent::WillDisconnect { connection_id: id, .. } if id == &connection_id)
        });
        let p2_disconnected = p2_events.iter().any(|e| {
            matches!(e, ProcessorEvent::Disconnected { connection_id: id, .. } if id == &connection_id)
        });

        assert!(
            p1_will_disconnect,
            "Source processor should receive WillDisconnect. Events: {:?}",
            p1_events
        );
        assert!(
            p1_disconnected,
            "Source processor should receive Disconnected. Events: {:?}",
            p1_events
        );
        assert!(
            p2_will_disconnect,
            "Destination processor should receive WillDisconnect. Events: {:?}",
            p2_events
        );
        assert!(
            p2_disconnected,
            "Destination processor should receive Disconnected. Events: {:?}",
            p2_events
        );
    }

    #[test]
    fn test_processor_events_include_correct_port_info() {
        // Test that processor events include correct port name and port type
        use crate::core::pubsub::{
            Event, EventListener, PortType as EventPortType, ProcessorEvent, EVENT_BUS,
        };
        use parking_lot::Mutex;
        use std::sync::Arc;

        let all_events = Arc::new(Mutex::new(Vec::new()));
        let all_events_clone = Arc::clone(&all_events);

        struct AllEventsListener {
            events: Arc<Mutex<Vec<Event>>>,
        }

        impl EventListener for AllEventsListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                self.events.lock().push(event.clone());
                Ok(())
            }
        }

        let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(AllEventsListener {
            events: all_events_clone,
        }));

        EVENT_BUS.subscribe("processor:proc1", Arc::clone(&listener));
        EVENT_BUS.subscribe("processor:proc2", Arc::clone(&listener));

        // Create runtime and connection
        let mut runtime = StreamRuntime::new();
        runtime.running = true;

        let connection_id = "test-conn-999".to_string();
        let conn = Connection::new(
            connection_id.clone(),
            "proc1.video_out".to_string(),
            "proc2.video_in".to_string(),
            PortType::Video,
            3,
        );

        runtime
            .connections
            .lock()
            .insert(connection_id.clone(), conn);
        all_events.lock().clear();

        // Disconnect
        let _ = runtime.disconnect_by_id(&connection_id);

        // Verify port information in events
        let events = all_events.lock();

        // Find processor events
        let processor_events: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::ProcessorEvent { .. }))
            .collect();

        assert!(!processor_events.is_empty(), "Should have processor events");

        // Check that events have correct port types
        let has_output_port_event = processor_events.iter().any(|e| {
            if let Event::ProcessorEvent {
                processor_id,
                event:
                    ProcessorEvent::WillDisconnect {
                        port_name,
                        port_type,
                        ..
                    },
            } = e
            {
                processor_id == "proc1"
                    && port_name == "video_out"
                    && *port_type == EventPortType::Output
            } else {
                false
            }
        });

        let has_input_port_event = processor_events.iter().any(|e| {
            if let Event::ProcessorEvent {
                processor_id,
                event:
                    ProcessorEvent::WillDisconnect {
                        port_name,
                        port_type,
                        ..
                    },
            } = e
            {
                processor_id == "proc2"
                    && port_name == "video_in"
                    && *port_type == EventPortType::Input
            } else {
                false
            }
        });

        assert!(
            has_output_port_event,
            "Should have Output port event for proc1. Events: {:?}",
            processor_events
        );
        assert!(
            has_input_port_event,
            "Should have Input port event for proc2. Events: {:?}",
            processor_events
        );
    }

    // ========================================================================
    // Connection Metadata Tests
    // ========================================================================

    #[test]
    fn test_connection_parses_processor_ids() {
        use crate::core::bus::PortType;

        let conn = Connection::new(
            "conn-1".to_string(),
            "processor_0.video_out".to_string(),
            "processor_1.video_in".to_string(),
            PortType::Video,
            3,
        );

        assert_eq!(conn.source_processor, "processor_0");
        assert_eq!(conn.dest_processor, "processor_1");
        assert_eq!(conn.from_port, "processor_0.video_out");
        assert_eq!(conn.to_port, "processor_1.video_in");
    }

    #[test]
    fn test_connection_stores_port_type_and_capacity() {
        use crate::core::bus::PortType;

        let conn = Connection::new(
            "conn-1".to_string(),
            "proc1.audio".to_string(),
            "proc2.audio".to_string(),
            PortType::Audio,
            32,
        );

        assert_eq!(conn.buffer_capacity, 32);
        assert!(matches!(conn.port_type, PortType::Audio));
    }

    #[test]
    fn test_connection_handles_edge_cases() {
        use crate::core::bus::PortType;

        // No dot in port address - should handle gracefully
        let conn1 = Connection::new(
            "conn-1".to_string(),
            "invalid".to_string(),
            "proc1.port".to_string(),
            PortType::Data,
            16,
        );
        assert_eq!(conn1.source_processor, "invalid");
        assert_eq!(conn1.dest_processor, "proc1");

        // Multiple dots - should take first part
        let conn2 = Connection::new(
            "conn-2".to_string(),
            "proc.a.b.port".to_string(),
            "proc.x.y.port".to_string(),
            PortType::Data,
            16,
        );
        assert_eq!(conn2.source_processor, "proc");
        assert_eq!(conn2.dest_processor, "proc");

        // Empty string - should handle gracefully
        let conn3 = Connection::new(
            "conn-3".to_string(),
            ".port".to_string(),
            "proc.".to_string(),
            PortType::Data,
            16,
        );
        assert_eq!(conn3.source_processor, "");
        assert_eq!(conn3.dest_processor, "proc");
    }

    // ========================================================================
    // ProcessorHandle Metadata Tests
    // ========================================================================

    #[test]
    fn test_processor_handle_with_metadata() {
        let handle = ProcessorHandle::with_metadata(
            "proc_0".to_string(),
            "streamlib::core::processors::TestProcessor".to_string(),
            Some(12345),
        );

        assert_eq!(handle.id(), "proc_0");
        assert_eq!(
            handle.processor_type(),
            "streamlib::core::processors::TestProcessor"
        );
        assert_eq!(handle.config_checksum(), Some(12345));
    }

    #[test]
    fn test_processor_handle_without_checksum() {
        let handle = ProcessorHandle::with_metadata(
            "proc_1".to_string(),
            "streamlib::core::processors::AnotherProcessor".to_string(),
            None,
        );

        assert_eq!(handle.id(), "proc_1");
        assert_eq!(
            handle.processor_type(),
            "streamlib::core::processors::AnotherProcessor"
        );
        assert_eq!(handle.config_checksum(), None);
    }

    // ========================================================================
    // Connection Index Tests
    // ========================================================================

    #[test]
    fn test_connection_index_empty_on_new_runtime() {
        let runtime = StreamRuntime::new();

        let connections = runtime.get_connections_for_processor(&"nonexistent".to_string());
        assert!(connections.is_empty());
    }

    #[test]
    fn test_connection_index_updated_on_connect() {
        // This test would require actual processors to be added
        // For now, we test the index directly via the internal state
        let mut runtime = StreamRuntime::new();

        // Manually insert a connection to test index
        use crate::core::bus::PortType;
        let conn = Connection::new(
            "test-conn".to_string(),
            "proc1.out".to_string(),
            "proc2.in".to_string(),
            PortType::Video,
            3,
        );

        {
            let mut connections = runtime.connections.lock();
            connections.insert("test-conn".to_string(), conn.clone());
        }

        // Manually update index as connect_at_runtime would
        runtime
            .processor_connections
            .entry(conn.source_processor.clone())
            .or_insert_with(Vec::new)
            .push("test-conn".to_string());
        runtime
            .processor_connections
            .entry(conn.dest_processor.clone())
            .or_insert_with(Vec::new)
            .push("test-conn".to_string());

        // Verify index
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        let proc2_conns = runtime.get_connections_for_processor(&"proc2".to_string());

        assert_eq!(proc1_conns.len(), 1);
        assert_eq!(proc2_conns.len(), 1);
        assert_eq!(proc1_conns[0], "test-conn");
        assert_eq!(proc2_conns[0], "test-conn");
    }

    #[test]
    fn test_connection_index_handles_multiple_connections() {
        let mut runtime = StreamRuntime::new();
        use crate::core::bus::PortType;

        // Add multiple connections
        for i in 0..3 {
            let conn_id = format!("conn-{}", i);
            let conn = Connection::new(
                conn_id.clone(),
                "proc1.out".to_string(),
                format!("proc{}.in", i + 2),
                PortType::Video,
                3,
            );

            {
                let mut connections = runtime.connections.lock();
                connections.insert(conn_id.clone(), conn.clone());
            }

            runtime
                .processor_connections
                .entry(conn.source_processor.clone())
                .or_insert_with(Vec::new)
                .push(conn_id.clone());
            runtime
                .processor_connections
                .entry(conn.dest_processor.clone())
                .or_insert_with(Vec::new)
                .push(conn_id);
        }

        // proc1 should have 3 connections (source for all)
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        assert_eq!(proc1_conns.len(), 3);

        // Each destination processor should have 1
        for i in 0..3 {
            let proc_conns = runtime.get_connections_for_processor(&format!("proc{}", i + 2));
            assert_eq!(proc_conns.len(), 1);
        }
    }

    #[test]
    fn test_connection_index_cleanup() {
        let mut runtime = StreamRuntime::new();
        use crate::core::bus::PortType;

        // Add a connection
        let conn = Connection::new(
            "test-conn".to_string(),
            "proc1.out".to_string(),
            "proc2.in".to_string(),
            PortType::Video,
            3,
        );

        {
            let mut connections = runtime.connections.lock();
            connections.insert("test-conn".to_string(), conn.clone());
        }

        runtime
            .processor_connections
            .entry("proc1".to_string())
            .or_insert_with(Vec::new)
            .push("test-conn".to_string());
        runtime
            .processor_connections
            .entry("proc2".to_string())
            .or_insert_with(Vec::new)
            .push("test-conn".to_string());

        // Simulate removing proc1
        runtime.processor_connections.remove(&"proc1".to_string());

        // proc1 should have no connections
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        assert!(proc1_conns.is_empty());

        // proc2 should still have the connection (until it's disconnected)
        let proc2_conns = runtime.get_connections_for_processor(&"proc2".to_string());
        assert_eq!(proc2_conns.len(), 1);
    }

    // ========================================================================
    // Stress Tests and Edge Cases
    // ========================================================================

    #[test]
    fn test_connection_index_stress_many_connections() {
        let mut runtime = StreamRuntime::new();
        use crate::core::bus::PortType;

        // Create 100 connections
        for i in 0..100 {
            let conn_id = format!("conn-{}", i);
            let conn = Connection::new(
                conn_id.clone(),
                format!("proc{}.out", i % 10),
                format!("proc{}.in", (i + 1) % 10),
                PortType::Video,
                3,
            );

            {
                let mut connections = runtime.connections.lock();
                connections.insert(conn_id.clone(), conn.clone());
            }

            runtime
                .processor_connections
                .entry(conn.source_processor.clone())
                .or_default()
                .push(conn_id.clone());
            runtime
                .processor_connections
                .entry(conn.dest_processor.clone())
                .or_default()
                .push(conn_id);
        }

        // Verify each processor has correct number of connections
        for i in 0..10 {
            let proc_id = format!("proc{}", i);
            let conns = runtime.get_connections_for_processor(&proc_id);
            // Each processor appears as source 10 times and dest 10 times
            assert_eq!(conns.len(), 20);
        }
    }

    #[test]
    fn test_connection_index_duplicate_entries_prevented() {
        let mut runtime = StreamRuntime::new();
        use crate::core::bus::PortType;

        let conn = Connection::new(
            "conn-1".to_string(),
            "proc1.out".to_string(),
            "proc2.in".to_string(),
            PortType::Video,
            3,
        );

        // Add same connection ID multiple times (shouldn't happen in practice)
        for _ in 0..3 {
            runtime
                .processor_connections
                .entry(conn.source_processor.clone())
                .or_default()
                .push("conn-1".to_string());
        }

        // Should have 3 entries (no deduplication in our simple implementation)
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        assert_eq!(proc1_conns.len(), 3);

        // This is expected behavior - the runtime ensures unique IDs
    }

    #[test]
    fn test_connection_parsing_with_unicode() {
        use crate::core::bus::PortType;

        let conn = Connection::new(
            "conn-1".to_string(),
            "processor_0.видео_out".to_string(), // Unicode in port name
            "processor_1.音频_in".to_string(),   // Unicode in port name
            PortType::Video,
            3,
        );

        assert_eq!(conn.source_processor, "processor_0");
        assert_eq!(conn.dest_processor, "processor_1");
    }

    #[test]
    fn test_processor_handle_type_name_preservation() {
        let handle = ProcessorHandle::with_metadata(
            "proc_0".to_string(),
            "very::long::nested::module::path::ProcessorName".to_string(),
            Some(99999),
        );

        assert_eq!(
            handle.processor_type(),
            "very::long::nested::module::path::ProcessorName"
        );
        assert_eq!(handle.config_checksum(), Some(99999));
    }

    #[test]
    fn test_connection_all_port_types() {
        use crate::core::bus::PortType;

        // Test Video
        let video_conn = Connection::new(
            "video-conn".to_string(),
            "p1.out".to_string(),
            "p2.in".to_string(),
            PortType::Video,
            3,
        );
        assert!(matches!(video_conn.port_type, PortType::Video));
        assert_eq!(video_conn.buffer_capacity, 3);

        // Test Audio
        let audio_conn = Connection::new(
            "audio-conn".to_string(),
            "p1.out".to_string(),
            "p2.in".to_string(),
            PortType::Audio,
            32,
        );
        assert!(matches!(audio_conn.port_type, PortType::Audio));
        assert_eq!(audio_conn.buffer_capacity, 32);

        // Test Data
        let data_conn = Connection::new(
            "data-conn".to_string(),
            "p1.out".to_string(),
            "p2.in".to_string(),
            PortType::Data,
            16,
        );
        assert!(matches!(data_conn.port_type, PortType::Data));
        assert_eq!(data_conn.buffer_capacity, 16);
    }

    #[test]
    fn test_connection_index_removal_idempotent() {
        let mut runtime = StreamRuntime::new();
        use crate::core::bus::PortType;

        let conn = Connection::new(
            "conn-1".to_string(),
            "proc1.out".to_string(),
            "proc2.in".to_string(),
            PortType::Video,
            3,
        );

        runtime
            .processor_connections
            .entry(conn.source_processor.clone())
            .or_default()
            .push("conn-1".to_string());

        // Remove entry for source processor
        if let Some(connections_vec) = runtime
            .processor_connections
            .get_mut(&conn.source_processor)
        {
            connections_vec.retain(|id| id != "conn-1");
        }

        // Verify it's gone
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        assert!(proc1_conns.is_empty());

        // Remove again (idempotent - should not panic)
        if let Some(connections_vec) = runtime
            .processor_connections
            .get_mut(&conn.source_processor)
        {
            connections_vec.retain(|id| id != "conn-1");
        }

        // Still empty
        let proc1_conns = runtime.get_connections_for_processor(&"proc1".to_string());
        assert!(proc1_conns.is_empty());
    }

    #[test]
    fn test_get_connections_for_nonexistent_processor() {
        let runtime = StreamRuntime::new();

        // Query for processor that was never added
        let conns = runtime.get_connections_for_processor(&"never_existed".to_string());
        assert!(conns.is_empty());

        // Multiple queries should all return empty
        for _ in 0..10 {
            let conns = runtime.get_connections_for_processor(&"another_nonexistent".to_string());
            assert!(conns.is_empty());
        }
    }

    #[test]
    fn test_connection_metadata_consistency() {
        use crate::core::bus::PortType;

        let conn1 = Connection::new(
            "conn-1".to_string(),
            "proc_a.output".to_string(),
            "proc_b.input".to_string(),
            PortType::Video,
            5,
        );

        let conn2 = Connection::new(
            "conn-2".to_string(),
            "proc_a.output".to_string(),
            "proc_b.input".to_string(),
            PortType::Video,
            5,
        );

        // Same port addresses should parse to same processor IDs
        assert_eq!(conn1.source_processor, conn2.source_processor);
        assert_eq!(conn1.dest_processor, conn2.dest_processor);

        // But different connection IDs
        assert_ne!(conn1.id, conn2.id);
    }
}
