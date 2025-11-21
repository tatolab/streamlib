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

#[derive(Debug, Clone)]
pub struct Connection {
    pub id: ConnectionId,
    pub from_port: String,
    pub to_port: String,
    pub created_at: std::time::Instant,
}

impl Connection {
    pub fn new(id: ConnectionId, from_port: String, to_port: String) -> Self {
        Self {
            id,
            from_port,
            to_port,
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
                processor_type,
            });
            EVENT_BUS.publish(&added_event.topic(), &added_event);
            tracing::debug!("[{}] Published RuntimeEvent::ProcessorAdded", id);
        }

        Ok(ProcessorHandle::new(id))
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
    ) -> Result<()> {
        let pending = PendingConnection::new(
            output.processor_id().clone(),
            output.port_name().to_string(),
            input.processor_id().clone(),
            input.port_name().to_string(),
        );

        self.pending_connections.push(pending.clone());

        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        let connection = Connection::new(
            connection_id.clone(),
            format!("{}.{}", output.processor_id(), output.port_name()),
            format!("{}.{}", input.processor_id(), input.port_name()),
        );

        {
            let mut connections = self.connections.lock();
            connections.insert(connection_id.clone(), connection);
        }

        tracing::debug!(
            "Registered pending connection: {} ({}.{} → {}.{})",
            connection_id,
            output.processor_id(),
            output.port_name(),
            input.processor_id(),
            input.port_name()
        );

        Ok(())
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

        tracing::info!(
            "Connecting {} ({}:{}) → ({}:{})",
            source,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port
        );

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
            PortType::Audio1 => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame<1>>(
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
            PortType::Audio2 => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame<2>>(
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
            PortType::Audio4 => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame<4>>(
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
            PortType::Audio6 => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame<6>>(
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
            PortType::Audio8 => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame<8>>(
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

        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        let connection = Connection::new(
            connection_id.clone(),
            source.to_string(),
            destination.to_string(),
        );

        {
            let mut connections = self.connections.lock();
            connections.insert(connection_id.clone(), connection);
        }

        tracing::info!("Registered runtime connection: {}", connection_id);
        Ok(connection_id)
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

        tracing::info!("[{}] Processor removed", processor_id);
        Ok(())
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
}
