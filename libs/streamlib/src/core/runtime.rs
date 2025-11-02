use super::clocks::{Clock, SoftwareClock};
use super::traits::{StreamProcessor, DynStreamProcessor};
use super::handles::{ProcessorHandle, PendingConnection};
use super::{Result, StreamError};
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

#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u32,
    pub buffer_size: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            buffer_size: 2048,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClockSource {
    Software {
        rate_hz: f64,
    },
}

struct TimerGroup {
    id: String,
    clock_source: ClockSource,
    processor_ids: Vec<ProcessorId>,
    wakeup_channels: Vec<crossbeam_channel::Sender<WakeupEvent>>,
    timer_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    Pending,
    Running,
    Stopping,
    Stopped,
}

type DynProcessor = Box<dyn DynStreamProcessor>;

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
    audio_config: AudioConfig,
    pending_connections: Vec<PendingConnection>,
    timer_groups: Arc<Mutex<HashMap<String, TimerGroup>>>,
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
            audio_config: AudioConfig::default(),
            pending_connections: Vec::new(),
            timer_groups: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn audio_config(&self) -> AudioConfig {
        self.audio_config
    }

    pub fn set_audio_config(&mut self, config: AudioConfig) {
        if self.running {
            tracing::warn!("Changing audio config while runtime is running may cause issues");
        }
        self.audio_config = config;
    }

    pub fn validate_audio_frame(&self, frame: &crate::core::AudioFrame) -> Result<()> {
        if frame.sample_rate != self.audio_config.sample_rate {
            return Err(StreamError::Configuration(format!(
                "AudioFrame sample rate mismatch: expected {}Hz (runtime config), got {}Hz. \
                 This can cause pitch shifts and audio artifacts. \
                 Ensure all audio processors use runtime.audio_config() when activating.",
                self.audio_config.sample_rate,
                frame.sample_rate
            )));
        }

        if frame.channels != self.audio_config.channels {
            return Err(StreamError::Configuration(format!(
                "AudioFrame channel count mismatch: expected {} channels (runtime config), got {} channels. \
                 This can cause audio artifacts. \
                 Ensure all audio processors use runtime.audio_config() when activating.",
                self.audio_config.channels,
                frame.channels
            )));
        }

        Ok(())
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
        processor: Box<dyn DynStreamProcessor>,
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

        let needs_timer = {
            let processor = processor_arc.lock();
            if let Some(descriptor) = processor.descriptor_instance_dyn() {
                descriptor.timer_requirements.is_some()
            } else {
                false
            }
        };

        if needs_timer {
            let processor = processor_arc.lock();
            if let Some(descriptor) = processor.descriptor_instance_dyn() {
                if let Some(timer_req) = descriptor.timer_requirements {
                    let wakeup_tx_timer = wakeup_tx.clone();
                    let id_for_timer = processor_id.clone();
                    let rate_hz = timer_req.rate_hz;

                    std::thread::spawn(move || {
                        let interval = std::time::Duration::from_secs_f64(1.0 / rate_hz);
                        tracing::info!("[{}] Timer thread started at {} Hz", id_for_timer, rate_hz);

                        loop {
                            std::thread::sleep(interval);
                            tracing::debug!("[{}] Timer thread sending TimerTick", id_for_timer);
                            if wakeup_tx_timer.send(WakeupEvent::TimerTick).is_err() {
                                tracing::debug!("[{}] Timer thread stopped (processor terminated)", id_for_timer);
                                break;
                            }
                        }
                    });
                }
            }
        }

        let id_for_thread = processor_id.clone();
        let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());
        let processor_for_thread = Arc::clone(&processor_arc);

        let handle = std::thread::spawn(move || {
            tracing::info!("[{}] Thread started", id_for_thread);

            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.on_start_dyn(&runtime_context) {
                    tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                    return;
                }
            }

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

    pub async fn connect_at_runtime(
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

        {
            let mut source = source_processor.lock();
            let mut dest = dest_processor.lock();

            let consumer = source
                .take_output_consumer_dyn(source_port)
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' does not have output port '{}'",
                        source_proc_id, source_port
                    ))
                })?;

            let connected = dest.connect_input_consumer_dyn(dest_port, consumer);

            if !connected {
                return Err(StreamError::Configuration(format!(
                    "Destination processor '{}' does not have input port '{}' or port type mismatch",
                    dest_proc_id, dest_port
                )));
            }

            tracing::info!(
                "Connected {} ({}) → {} ({}) via rtrb ring buffer",
                source_proc_id,
                source_port,
                dest_proc_id,
                dest_port
            );
        }

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

            tracing::info!(
                "Wiring connection: {} → {}",
                source,
                destination
            );

            let (source_processor, dest_processor) = {
                let processors = self.processors.lock();

                let source_handle = processors.get(&pending.source_processor_id).ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' not found",
                        pending.source_processor_id
                    ))
                })?;

                let dest_handle = processors.get(&pending.dest_processor_id).ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Destination processor '{}' not found",
                        pending.dest_processor_id
                    ))
                })?;

                let source_proc = source_handle.processor.as_ref().ok_or_else(|| {
                    StreamError::Runtime(format!(
                        "Source processor '{}' has no processor reference (not started?)",
                        pending.source_processor_id
                    ))
                })?;

                let dest_proc = dest_handle.processor.as_ref().ok_or_else(|| {
                    StreamError::Runtime(format!(
                        "Destination processor '{}' has no processor reference (not started?)",
                        pending.dest_processor_id
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
                            pending.source_processor_id, pending.dest_processor_id
                        );
                    }
                }
            }

            {
                let mut source = source_processor.lock();
                let mut dest = dest_processor.lock();

                let consumer = source
                    .take_output_consumer_dyn(&pending.source_port_name)
                    .ok_or_else(|| {
                        StreamError::Configuration(format!(
                            "Source processor '{}' does not have output port '{}'",
                            pending.source_processor_id, pending.source_port_name
                        ))
                    })?;

                let connected = dest.connect_input_consumer_dyn(&pending.dest_port_name, consumer);

                if !connected {
                    return Err(StreamError::Configuration(format!(
                        "Destination processor '{}' does not have input port '{}' or port type mismatch",
                        pending.dest_processor_id, pending.dest_port_name
                    )));
                }

                tracing::info!(
                    "Wired connection: {} ({}) → {} ({}) via rtrb ring buffer",
                    pending.source_processor_id,
                    pending.source_port_name,
                    pending.dest_processor_id,
                    pending.dest_port_name
                );
            }

            {
                let processors = self.processors.lock();

                if let Some(dest_handle) = processors.get(&pending.dest_processor_id) {
                    let dest_wakeup_tx = dest_handle.wakeup_tx.clone();

                    let mut source = source_processor.lock();
                    source.set_output_wakeup_dyn(&pending.source_port_name, dest_wakeup_tx);

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

        tracing::info!("All pending connections wired successfully");
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

        let mut timer_groups_map: HashMap<String, Vec<(ProcessorId, f64, crossbeam_channel::Sender<WakeupEvent>)>> = HashMap::new();
        let mut solo_timers: Vec<(ProcessorId, f64, crossbeam_channel::Sender<WakeupEvent>)> = Vec::new();

        for (processor_id, processor, shutdown_rx) in self.pending_processors.drain(..) {
            let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

            let processor_arc = Arc::new(Mutex::new(processor));

            {
                let mut processor = processor_arc.lock();
                processor.set_wakeup_channel_dyn(wakeup_tx.clone());
            }

            {
                let processor = processor_arc.lock();
                if let Some(descriptor) = processor.descriptor_instance_dyn() {
                    if let Some(timer_req) = descriptor.timer_requirements {
                        let rate_hz = timer_req.rate_hz;
                        let wakeup_tx_timer = wakeup_tx.clone();

                        if let Some(group_id) = timer_req.group_id {
                            timer_groups_map.entry(group_id)
                                .or_default()
                                .push((processor_id.clone(), rate_hz, wakeup_tx_timer));
                        } else {
                            solo_timers.push((processor_id.clone(), rate_hz, wakeup_tx_timer));
                        }
                    }
                }
            }

            let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());

            let id_for_thread = processor_id.clone();

            let processor_for_thread = Arc::clone(&processor_arc);

            let handle = std::thread::spawn(move || {
                tracing::info!("[{}] Thread started", id_for_thread);

                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.on_start_dyn(&runtime_context) {
                        tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                        return;
                    }
                }

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

        for (group_id, members) in timer_groups_map {
            let rates: Vec<f64> = members.iter().map(|(_, rate, _)| *rate).collect();
            let first_rate = rates[0];
            if !rates.iter().all(|&r| (r - first_rate).abs() < 0.01) {
                return Err(StreamError::Configuration(
                    format!("Timer group '{}' has mismatched rates: {:?}", group_id, rates)
                ));
            }

            let rate_hz = first_rate;
            let processor_ids: Vec<ProcessorId> = members.iter().map(|(id, _, _)| id.clone()).collect();
            let wakeup_channels: Vec<crossbeam_channel::Sender<WakeupEvent>> =
                members.iter().map(|(_, _, tx)| tx.clone()).collect();

            tracing::info!(
                "[TimerGroup:{}] Created with {} processors at {:.2} Hz: {:?}",
                group_id, processor_ids.len(), rate_hz, processor_ids
            );

            let group_id_clone = group_id.clone();
            let wakeup_channels_clone = wakeup_channels.clone();
            let timer_thread = std::thread::spawn(move || {
                let interval = std::time::Duration::from_secs_f64(1.0 / rate_hz);
                let mut tick_count = 0u64;

                tracing::info!("[TimerGroup:{}] Timer thread started at {:.2} Hz", group_id_clone, rate_hz);

                loop {
                    std::thread::sleep(interval);
                    tick_count += 1;

                    let mut active_count = 0;
                    for tx in &wakeup_channels_clone {
                        if tx.send(WakeupEvent::TimerTick).is_ok() {
                            active_count += 1;
                        }
                    }

                    if active_count == 0 {
                        tracing::info!("[TimerGroup:{}] All processors terminated, stopping timer", group_id_clone);
                        break;
                    }

                    if tick_count % 100 == 0 {
                        tracing::debug!(
                            "[TimerGroup:{}] Tick #{}, {} active processors",
                            group_id_clone, tick_count, active_count
                        );
                    }
                }
            });

            let mut groups = self.timer_groups.lock();
            groups.insert(group_id.clone(), TimerGroup {
                id: group_id,
                clock_source: ClockSource::Software { rate_hz },
                processor_ids,
                wakeup_channels,
                timer_thread: Some(timer_thread),
            });
        }

        for (processor_id, rate_hz, wakeup_tx) in solo_timers {
            let processor_id_clone = processor_id.clone();
            std::thread::spawn(move || {
                let interval = std::time::Duration::from_secs_f64(1.0 / rate_hz);
                tracing::info!("[Timer:{}] Solo timer started at {:.2} Hz", processor_id_clone, rate_hz);

                loop {
                    std::thread::sleep(interval);
                    if wakeup_tx.send(WakeupEvent::TimerTick).is_err() {
                        tracing::debug!("[Timer:{}] Timer stopped (processor terminated)", processor_id_clone);
                        break;
                    }
                }
            });
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
                .with_timer_requirements(schema::TimerRequirements {
                    rate_hz: 60.0,
                    group_id: None,
                    description: Some("Counter test processor".to_string()),
                })
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
                    .with_timer_requirements(schema::TimerRequirements {
                        rate_hz: 60.0,
                        group_id: None,
                        description: Some("Work test processor".to_string()),
                    })
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
    fn test_audio_config_validation() {
        let runtime = StreamRuntime::new();

        let matching_frame = crate::core::AudioFrame::new(
            vec![0.0; 2048],
            0,
            0,
            48000,
            2,
        );
        assert!(runtime.validate_audio_frame(&matching_frame).is_ok());

        let wrong_sample_rate_frame = crate::core::AudioFrame::new(
            vec![0.0; 2048],
            0,
            0,
            44100,
            2,
        );
        let result = runtime.validate_audio_frame(&wrong_sample_rate_frame);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sample rate mismatch"));

        let wrong_channels_frame = crate::core::AudioFrame::new(
            vec![0.0; 1024],
            0,
            0,
            48000,
            1,
        );
        let result = runtime.validate_audio_frame(&wrong_channels_frame);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("channel count mismatch"));
    }

    #[test]
    fn test_audio_config_getter_setter() {
        let mut runtime = StreamRuntime::new();

        let config = runtime.audio_config();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
        assert_eq!(config.buffer_size, 2048);

        runtime.set_audio_config(AudioConfig {
            sample_rate: 44100,
            channels: 1,
            buffer_size: 1024,
        });

        let new_config = runtime.audio_config();
        assert_eq!(new_config.sample_rate, 44100);
        assert_eq!(new_config.channels, 1);
        assert_eq!(new_config.buffer_size, 1024);
    }
}
