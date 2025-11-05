use super::traits::{StreamProcessor, DynStreamElement};
use super::handles::{ProcessorHandle, PendingConnection};
use super::{Result, StreamError};
use super::ports::PortType;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use parking_lot::Mutex;
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

pub use crate::core::context::AudioContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    Pending,
    Running,
    Stopping,
    Stopped,
}

type DynProcessor = Box<dyn DynStreamElement>;

pub(crate) struct RuntimeProcessorHandle {
    pub id: ProcessorId,
    pub name: String,
    thread: Option<JoinHandle<()>>,
    shutdown_tx: crossbeam_channel::Sender<()>,
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

pub type EventLoopFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send>;

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
    audio_context: AudioContext,
    pending_connections: Vec<PendingConnection>,
    bus: crate::core::Bus,
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
            audio_context: AudioContext::default(),
            bus: crate::core::Bus::new(),
            pending_connections: Vec::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn audio_config(&self) -> AudioContext {
        self.audio_context
    }

    pub fn set_audio_config(&mut self, config: AudioContext) {
        if self.running {
            tracing::warn!("Changing audio config while runtime is running may cause issues");
        }
        self.audio_context = config;
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
        if self.running {
            return Err(StreamError::Runtime(
                "Cannot add processor while runtime is running. Use add_processor_runtime() instead.".into()
            ));
        }

        let processor = P::from_config(config)?;

        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

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

        self.pending_processors.push((id.clone(), Box::new(processor) as DynProcessor, shutdown_rx));

        tracing::info!("Added processor with ID: {}", id);

        Ok(ProcessorHandle::new(id))
    }

    pub async fn add_processor_runtime(
        &mut self,
        processor: Box<dyn DynStreamElement>,
    ) -> Result<ProcessorId> {
        if !self.running {
            return Err(StreamError::Runtime(
                "Cannot add processor at runtime - runtime is not running. Use add_processor() instead.".into()
            ));
        }

        let processor_id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        tracing::info!("[{}] Adding processor to running runtime...", processor_id);

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
            processor.set_wakeup_channel_dyn(wakeup_tx.clone());
        }

        let id_for_thread = processor_id.clone();
        let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());
        let processor_for_thread = Arc::clone(&processor_arc);

        let sched_config = {
            let processor = processor_arc.lock();
            processor.scheduling_config_dyn()
        };

        let handle = std::thread::spawn(move || {
            tracing::info!("[{}] Thread started (mode: {:?})", id_for_thread, sched_config.mode);

            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.on_start_dyn(&runtime_context) {
                    tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                    return;
                }
            }

            match sched_config.mode {
                crate::core::scheduling::SchedulingMode::Pull => {
                    tracing::info!("[{}] Pull mode - processor manages own callback, waiting for shutdown", id_for_thread);

                    let _ = shutdown_rx.recv();
                    tracing::info!("[{}] Shutdown signal received (pull mode)", id_for_thread);
                }

                crate::core::scheduling::SchedulingMode::Loop => {
                    loop {
                        match shutdown_rx.try_recv() {
                            Ok(_) => {
                                tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                break;
                            }
                            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                tracing::warn!("[{}] Shutdown channel closed", id_for_thread);
                                break;
                            }
                            Err(crossbeam_channel::TryRecvError::Empty) => {
                            }
                        }

                        {
                            let mut processor = processor_for_thread.lock();
                            if let Err(e) = processor.process_dyn() {
                                tracing::error!("[{}] process() error (loop mode): {}", id_for_thread, e);
                            }
                        }

                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                }

                crate::core::scheduling::SchedulingMode::Push => {
                    loop {
                        crossbeam_channel::select! {
                            recv(wakeup_rx) -> result => {
                                match result {
                                    Ok(WakeupEvent::DataAvailable) => {
                                        tracing::debug!("[{}] Received DataAvailable wakeup", id_for_thread);
                                        let mut processor = processor_for_thread.lock();
                                        if let Err(e) = processor.process_dyn() {
                                            tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                        }
                                    }
                                    Ok(WakeupEvent::TimerTick) => {
                                        tracing::debug!("[{}] Received TimerTick wakeup", id_for_thread);
                                        let mut processor = processor_for_thread.lock();
                                        if let Err(e) = processor.process_dyn() {
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
                    }
                }
            }

            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.on_stop_dyn() {
                    tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                }
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

        tracing::info!("[{}] Processor added to running runtime", processor_id);
        Ok(processor_id)
    }

    pub fn connect<T: crate::core::ports::PortMessage>(
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
            output.processor_id(), output.port_name(),
            input.processor_id(), input.port_name()
        );

        Ok(())
    }

    pub fn connect_at_runtime(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<ConnectionId> {
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
                StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
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

            let source_descriptor = source_guard.descriptor_instance_dyn();
            let dest_descriptor = dest_guard.descriptor_instance_dyn();

            if let (Some(source_desc), Some(dest_desc)) = (source_descriptor, dest_descriptor) {
                if let (Some(source_audio), Some(dest_audio)) =
                    (&source_desc.audio_requirements, &dest_desc.audio_requirements)
                {
                    if !source_audio.compatible_with(dest_audio) {
                        let error_msg = source_audio.compatibility_error(dest_audio);
                        return Err(StreamError::Configuration(format!(
                            "Audio requirements incompatible when connecting {} → {}: {}",
                            source, destination, error_msg
                        )));
                    }

                    tracing::debug!(
                        "Audio requirements validated: {} → {} (compatible)",
                        source_proc_id, dest_proc_id
                    );
                }
            }
        }

        let (source_port_type, dest_port_type) = {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            let src_type = source_guard.get_output_port_type(source_port)
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' does not have output port '{}'",
                        source_proc_id, source_port
                    ))
                })?;

            let dst_type = dest_guard.get_input_port_type(dest_port)
                .ok_or_else(|| {
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

        let connection: Arc<dyn std::any::Any + Send + Sync> = match source_port_type {
            PortType::Audio1 => {
                let conn = self.bus.create_audio_connection::<1>(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Audio2 => {
                let conn = self.bus.create_audio_connection::<2>(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Audio4 => {
                let conn = self.bus.create_audio_connection::<4>(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Audio6 => {
                let conn = self.bus.create_audio_connection::<6>(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Audio8 => {
                let conn = self.bus.create_audio_connection::<8>(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Video => {
                let conn = self.bus.create_video_connection(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
            PortType::Data => {
                let conn = self.bus.create_data_connection(
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                    source_port_type.default_capacity(),
                );
                Arc::new(conn) as Arc<dyn std::any::Any + Send + Sync>
            },
        };

        {
            let mut source_guard = source_processor.lock();
            let success = source_guard.wire_output_connection(source_port, connection.clone());
            if !success {
                return Err(StreamError::Configuration(format!(
                    "Failed to wire connection to output port '{}' on processor '{}'",
                    source_port, source_proc_id
                )));
            }
        }

        {
            let mut dest_guard = dest_processor.lock();
            let success = dest_guard.wire_input_connection(dest_port, connection);
            if !success {
                return Err(StreamError::Configuration(format!(
                    "Failed to wire connection to input port '{}' on processor '{}'",
                    dest_port, dest_proc_id
                )));
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

        let connection = Connection::new(connection_id.clone(), source.to_string(), destination.to_string());

        {
            let mut connections = self.connections.lock();
            connections.insert(connection_id.clone(), connection);
        }

        tracing::info!("Registered runtime connection: {}", connection_id);
        Ok(connection_id)
    }

    async fn wire_pending_connections(&mut self) -> Result<()> {
        if self.pending_connections.is_empty() {
            tracing::debug!("No pending connections to wire");
            return Ok(());
        }

        tracing::info!("Wiring {} pending connections...", self.pending_connections.len());

        let connections_to_wire = std::mem::take(&mut self.pending_connections);

        for pending in connections_to_wire {
            let source = format!("{}.{}", pending.source_processor_id, pending.source_port_name);
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
                        source_guard.set_output_wakeup_dyn(&pending.source_port_name, dst.wakeup_tx.clone());

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
                    let sched_config = proc_ref.lock().scheduling_config_dyn();
                    if matches!(sched_config.mode, crate::core::scheduling::SchedulingMode::Pull) {
                        if let Err(e) = handle.wakeup_tx.send(WakeupEvent::DataAvailable) {
                            tracing::warn!("[{}] Failed to send Pull mode initialization wakeup: {}", proc_id, e);
                        } else {
                            tracing::debug!("[{}] Sent Pull mode initialization wakeup", proc_id);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.running {
            return Err(StreamError::Configuration("Runtime already running".into()));
        }

        let handler_count = self.pending_processors.len();

        tracing::info!(
            "Starting runtime with {} processors",
            handler_count
        );

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
        let gpu_context = crate::core::context::GpuContext::init_for_platform().await?;
        tracing::info!("GPU context initialized: {:?}", gpu_context);
        self.gpu_context = Some(gpu_context);

        self.running = true;

        self.spawn_handler_threads()?;

        self.wire_pending_connections().await?;

        tracing::info!("Runtime started successfully");
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        if !self.running {
            self.start().await?;
        }

        tracing::info!("Runtime running (press Ctrl+C to stop)");

        if let Some(event_loop) = self.event_loop.take() {
            tracing::debug!("Using platform-specific event loop");
            event_loop().await?;
        } else {
            tokio::signal::ctrl_c().await.map_err(|e| {
                StreamError::Configuration(format!("Failed to listen for shutdown signal: {}", e))
            })?;

            tracing::info!("Shutdown signal received");
        }

        self.stop().await?;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }

        tracing::info!("Stopping runtime...");
        self.running = false;

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
                        tracing::debug!("[{}] Thread joined ({}/{})", processor_id, i + 1, thread_count);
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

    pub async fn remove_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        let shutdown_tx = {
            let mut processors = self.processors.lock();
            let processor = processors.get_mut(processor_id).ok_or_else(|| {
                StreamError::NotFound(format!("Processor '{}' not found", processor_id))
            })?;

            let current_status = *processor.status.lock();
            if current_status == ProcessorStatus::Stopped || current_status == ProcessorStatus::Stopping {
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
            let join_result = tokio::task::spawn_blocking(move || {
                handle.join()
            }).await;

            match join_result {
                Ok(Ok(_)) => {
                    tracing::info!("[{}] Processor thread joined successfully", processor_id);

                    let mut processors = self.processors.lock();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock() = ProcessorStatus::Stopped;
                    }
                }
                Ok(Err(panic_err)) => {
                    tracing::error!("[{}] Processor thread panicked: {:?}", processor_id, panic_err);

                    let mut processors = self.processors.lock();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock() = ProcessorStatus::Stopped;
                    }

                    return Err(StreamError::Runtime(format!(
                        "Processor '{}' thread panicked",
                        processor_id
                    )));
                }
                Err(join_err) => {
                    tracing::error!("[{}] Failed to join thread: {:?}", processor_id, join_err);
                    return Err(StreamError::Runtime(format!(
                        "Failed to join processor '{}' thread",
                        processor_id
                    )));
                }
            }
        } else {
            tracing::warn!("[{}] No thread handle found (processor may not have started)", processor_id);
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
                processor.set_wakeup_channel_dyn(wakeup_tx.clone());
            }

            let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());

            let id_for_thread = processor_id.clone();

            let processor_for_thread = Arc::clone(&processor_arc);

            let sched_config = {
                let processor = processor_arc.lock();
                processor.scheduling_config_dyn()
            };

            let handle = std::thread::spawn(move || {
                tracing::info!("[{}] Thread started (mode: {:?})", id_for_thread, sched_config.mode);

                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.on_start_dyn(&runtime_context) {
                        tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                        return;
                    }
                }

                match sched_config.mode {
                    crate::core::scheduling::SchedulingMode::Pull => {
                        tracing::info!("[{}] Pull mode - waiting for connections to be wired", id_for_thread);

                        let init_result = crossbeam_channel::select! {
                            recv(wakeup_rx) -> result => {
                                match result {
                                    Ok(_) => {
                                        tracing::info!("[{}] Pull mode - connections ready, calling process() for initialization", id_for_thread);
                                        let mut processor = processor_for_thread.lock();
                                        processor.process_dyn()
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
                            tracing::error!("[{}] Pull mode process() initialization failed: {}", id_for_thread, e);
                            return;
                        }

                        tracing::info!("[{}] Pull mode initialized - processor manages own callback, waiting for shutdown", id_for_thread);

                        let _ = shutdown_rx.recv();
                        tracing::info!("[{}] Shutdown signal received (pull mode)", id_for_thread);
                    }

                    crate::core::scheduling::SchedulingMode::Loop => {
                        loop {
                            match shutdown_rx.try_recv() {
                                Ok(_) => {
                                    tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                    break;
                                }
                                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                    tracing::warn!("[{}] Shutdown channel closed", id_for_thread);
                                    break;
                                }
                                Err(crossbeam_channel::TryRecvError::Empty) => {
                                }
                            }

                            {
                                let mut processor = processor_for_thread.lock();
                                if let Err(e) = processor.process_dyn() {
                                    tracing::error!("[{}] process() error (loop mode): {}", id_for_thread, e);
                                }
                            }

                            std::thread::sleep(std::time::Duration::from_micros(10));
                        }
                    }

                    crate::core::scheduling::SchedulingMode::Push => {
                        loop {
                            crossbeam_channel::select! {
                                recv(wakeup_rx) -> result => {
                                    match result {
                                        Ok(WakeupEvent::DataAvailable) => {
                                            let mut processor = processor_for_thread.lock();
                                            if let Err(e) = processor.process_dyn() {
                                                tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                            }
                                        }
                                        Ok(WakeupEvent::TimerTick) => {
                                            let mut processor = processor_for_thread.lock();
                                            if let Err(e) = processor.process_dyn() {
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
                        }
                    }
                }

                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.on_stop_dyn() {
                        tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                    }
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
    use crate::core::traits::StreamProcessor;
    use crate::core::{schema, ProcessorDescriptor};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone)]
    struct CounterConfig {
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
        count: Arc<AtomicU64>,
    }

    impl StreamProcessor for CounterProcessor {
        type Config = CounterConfig;

        fn from_config(config: Self::Config) -> Result<Self> {
            Ok(Self { count: config.count })
        }

        fn process(&mut self) -> Result<()> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            Some(
                ProcessorDescriptor::new(
                    "CounterProcessor",
                    "Test processor that increments a counter"
                )
            )
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
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

        let _handle = runtime.add_processor_with_config::<CounterProcessor>(config).unwrap();
        assert_eq!(runtime.pending_processors.len(), 1);
    }

    #[tokio::test]
    async fn test_runtime_lifecycle() {
        let mut runtime = StreamRuntime::new();

        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));

        let config1 = CounterConfig { count: Arc::clone(&count1) };
        let config2 = CounterConfig { count: Arc::clone(&count2) };

        runtime.add_processor_with_config::<CounterProcessor>(config1).unwrap();
        runtime.add_processor_with_config::<CounterProcessor>(config2).unwrap();

        runtime.start().await.unwrap();
        assert!(runtime.running);

        {
            let processors = runtime.processors.lock();
            assert_eq!(processors.len(), 2);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        runtime.stop().await.unwrap();
        assert!(!runtime.running);

        let c1 = count1.load(Ordering::Relaxed);
        let c2 = count2.load(Ordering::Relaxed);

        assert!(c1 > 0);
        assert!(c2 > 0);
    }

    #[tokio::test]
    async fn test_true_parallelism() {
        use std::time::Instant;

        #[derive(Clone)]
        struct WorkConfig {
            work_duration_ms: u64,
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
            work_duration_ms: u64,
            start_times: Arc<Mutex<Vec<Instant>>>,
            work_counter: u64,
        }

        impl StreamProcessor for WorkProcessor {
            type Config = WorkConfig;

            fn from_config(config: Self::Config) -> Result<Self> {
                Ok(Self {
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
                Some(
                    ProcessorDescriptor::new(
                        "WorkProcessor",
                        "Test processor that performs CPU work"
                    )
                )
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
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

        runtime.add_processor_with_config::<WorkProcessor>(config1).unwrap();
        runtime.add_processor_with_config::<WorkProcessor>(config2).unwrap();

        runtime.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(350)).await;

        runtime.stop().await.unwrap();

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
    fn test_audio_config_getter_setter() {
        let mut runtime = StreamRuntime::new();

        let config = runtime.audio_config();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.buffer_size, 128); // AudioContext default

        runtime.set_audio_config(AudioContext {
            sample_rate: 44100,
            buffer_size: 1024,
        });

        let new_config = runtime.audio_config();
        assert_eq!(new_config.sample_rate, 44100);
        assert_eq!(new_config.buffer_size, 1024);
    }
}
