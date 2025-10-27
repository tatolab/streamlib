//! StreamRuntime - Wires clock, broadcaster, and handlers together
//!
//! This module implements the runtime that manages the complete streaming pipeline:
//! - Clock generates ticks at fixed rate (async tokio task)
//! - TickBroadcaster distributes ticks to all handlers (non-blocking)
//! - Handlers run in OS threads, process ticks independently
//!
//! # Architecture
//!
//! ```text
//! Clock Task (tokio)          TickBroadcaster           Handler Threads (OS)
//!       │                            │                           │
//!       ├─ tick()──────────────────►│                           │
//!       │                            ├─ broadcast()────────────►│ Handler 1
//!       │                            │                           │  └─ process(tick)
//!       │                            ├─ broadcast()────────────►│ Handler 2
//!       │                            │                           │  └─ process(tick)
//!       │                            ├─ broadcast()────────────►│ Handler 3
//!       │                            │                           │  └─ process(tick)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use streamlib_core::{StreamRuntime, Stream, StreamHandler, TimedTick, Result};
//!
//! // Create runtime at 60fps
//! let mut runtime = StreamRuntime::new(60.0);
//!
//! // Add handlers
//! runtime.add_stream(Stream::new(Box::new(camera_handler)));
//! runtime.add_stream(Stream::new(Box::new(blur_handler)));
//! runtime.add_stream(Stream::new(Box::new(display_handler)));
//!
//! // Connect handlers
//! runtime.connect(camera_handler.id(), "video", blur_handler.id(), "video");
//! runtime.connect(blur_handler.id(), "video", display_handler.id(), "video");
//!
//! // Start runtime (spawns clock + handler threads)
//! runtime.start().await?;
//!
//! // Run until stopped
//! runtime.run().await?;
//! ```

use super::clock::{Clock, SoftwareClock};
use super::events::TickBroadcaster;
use super::stream_processor::StreamProcessor;
use super::{Result, StreamError};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Opaque shader ID (for future GPU operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

/// Unique identifier for processors in the runtime
pub type ProcessorId = String;

/// Status of a processor in the runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorStatus {
    /// Processor added but not yet started
    Pending,
    /// Processor thread is running
    Running,
    /// Shutdown signal sent, thread stopping
    Stopping,
    /// Processor thread has stopped
    Stopped,
}

/// Handle to a running processor
pub struct ProcessorHandle {
    /// Unique processor ID
    pub id: ProcessorId,

    /// Human-readable processor name
    pub name: String,

    /// Processor thread handle (None if not started yet)
    thread: Option<JoinHandle<()>>,

    /// Channel for sending shutdown signal to processor thread
    shutdown_tx: crossbeam_channel::Sender<()>,

    /// Current processor status
    pub(crate) status: Arc<Mutex<ProcessorStatus>>,

    /// Shared reference to the processor (for dynamic connections at runtime)
    /// Wrapped in Arc<Mutex<>> so it can be accessed from both the processing thread
    /// and connection operations
    pub(crate) processor: Option<Arc<Mutex<Box<dyn StreamProcessor>>>>,
}

/// Unique identifier for connections
pub type ConnectionId = String;

/// Represents a connection between two ports
///
/// This tracks port wiring in a type-erased way. The actual connection
/// is maintained by the port buffer references, but we need this registry
/// to support dynamic graph operations (add/remove connections at runtime).
#[derive(Debug, Clone)]
pub struct Connection {
    /// Unique connection ID
    pub id: ConnectionId,

    /// Source port identifier (format: "processor_id.port_name")
    /// For now, this is opaque until we add processor port metadata
    pub from_port: String,

    /// Destination port identifier (format: "processor_id.port_name")
    pub to_port: String,

    /// Timestamp when connection was created
    pub created_at: std::time::Instant,
}

impl Connection {
    /// Create a new connection
    pub fn new(id: ConnectionId, from_port: String, to_port: String) -> Self {
        Self {
            id,
            from_port,
            to_port,
            created_at: std::time::Instant::now(),
        }
    }
}

/// Platform-specific event loop hook
///
/// Platforms can provide a custom event loop that runs alongside the runtime.
/// This is used by macOS to process NSApplication events on the main thread.
pub type EventLoopFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send>;

/// StreamRuntime - Manages clock, handlers, and connections
///
/// The runtime is the central coordinator that:
/// 1. Owns the clock (generates ticks)
/// 2. Owns the broadcaster (distributes ticks)
/// 3. Spawns handler threads (processes ticks)
/// 4. Manages connections between handlers
///
/// # Threading Model
///
/// - **Clock**: Runs in tokio task (async, single thread)
/// - **Handlers**: Each runs in OS thread (true parallelism)
/// - **Broadcaster**: Lock-free channels (crossbeam)
///
/// # Lifecycle
///
/// 1. Create runtime: `StreamRuntime::new(fps)`
/// 2. Add streams: `runtime.add_stream(stream)`
/// 3. Connect ports: `runtime.connect(...)`
/// 4. Start: `runtime.start().await` - spawns threads
/// 5. Run: `runtime.run().await` - blocks until stopped
/// 6. Stop: `runtime.stop().await` - clean shutdown
pub struct StreamRuntime {
    /// Clock for generating ticks (will be moved to tokio task on start)
    clock: Option<Box<dyn Clock + Send>>,

    /// FPS for runtime (stored for status/debugging)
    fps: f64,

    /// Broadcaster for distributing ticks to handlers
    broadcaster: Arc<Mutex<TickBroadcaster>>,

    /// Processor registry (maps ID -> ProcessorHandle)
    /// Tracks all processors (pending, running, stopped)
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, ProcessorHandle>>>,

    /// Processors waiting to be started (drained on start)
    /// Stores: (processor_id, processor, shutdown_receiver)
    /// TODO: Remove after full migration to processor registry
    pending_processors: Vec<(ProcessorId, Box<dyn StreamProcessor>, crossbeam_channel::Receiver<()>)>,

    /// Handler threads (spawned on start)
    /// TODO: Remove after full migration to processor registry
    handler_threads: Vec<JoinHandle<()>>,

    /// Clock task handle (spawned on start)
    clock_task: Option<tokio::task::JoinHandle<()>>,

    /// Running flag
    running: bool,

    /// Optional platform-specific event loop hook
    event_loop: Option<EventLoopFn>,

    /// GPU context (shared device/queue for all processors)
    /// Initialized automatically during start()
    gpu_context: Option<crate::core::gpu_context::GpuContext>,

    /// Counter for generating unique processor IDs
    next_processor_id: usize,

    /// Connection registry (maps ID -> Connection)
    /// Tracks all port connections for dynamic graph operations
    pub(crate) connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,

    /// Counter for generating unique connection IDs
    next_connection_id: usize,
}

impl StreamRuntime {
    /// Create a new runtime with software clock
    ///
    /// # Arguments
    ///
    /// * `fps` - Target frames per second for clock
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib_core::StreamRuntime;
    ///
    /// let runtime = StreamRuntime::new(60.0);
    /// ```
    pub fn new(fps: f64) -> Self {
        Self {
            clock: Some(Box::new(SoftwareClock::new(fps))),
            fps,
            broadcaster: Arc::new(Mutex::new(TickBroadcaster::new())),
            processors: Arc::new(Mutex::new(HashMap::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            clock_task: None,
            running: false,
            event_loop: None,
            gpu_context: None,
            next_processor_id: 0,
            connections: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: 0,
        }
    }

    /// Create runtime with custom clock
    ///
    /// Use this to provide PTP or Genlock clocks instead of software clock.
    ///
    /// # Arguments
    ///
    /// * `clock` - Custom clock implementation
    /// * `fps` - Nominal FPS (for status reporting)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib_core::{StreamRuntime, PTPClock};
    ///
    /// let ptp_clock = PTPClock::new(60.0);
    /// let runtime = StreamRuntime::with_clock(Box::new(ptp_clock), 60.0);
    /// ```
    pub fn with_clock(clock: Box<dyn Clock + Send>, fps: f64) -> Self {
        Self {
            clock: Some(clock),
            fps,
            broadcaster: Arc::new(Mutex::new(TickBroadcaster::new())),
            processors: Arc::new(Mutex::new(HashMap::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            clock_task: None,
            running: false,
            event_loop: None,
            gpu_context: None,
            next_processor_id: 0,
            connections: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: 0,
        }
    }

    /// Set a platform-specific event loop hook
    ///
    /// This allows platforms (like macOS) to inject their own event processing
    /// that runs alongside the runtime. The event loop will be called during `run()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.set_event_loop(Box::new(|| {
    ///     Box::pin(async { /* platform event loop */ Ok(()) })
    /// }));
    /// ```
    pub fn set_event_loop(&mut self, event_loop: EventLoopFn) {
        self.event_loop = Some(event_loop);
    }

    /// Get the GPU context (if initialized)
    ///
    /// Returns None before start() is called.
    pub fn gpu_context(&self) -> Option<&crate::core::gpu_context::GpuContext> {
        self.gpu_context.as_ref()
    }

    /// Add a processor to the runtime
    ///
    /// Processors are held until `start()` is called, then spawned into threads.
    ///
    /// # Arguments
    ///
    /// * `processor` - Boxed processor implementation
    ///
    /// # Returns
    ///
    /// The generated ProcessorId for this processor
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib_core::StreamRuntime;
    ///
    /// let mut runtime = StreamRuntime::new(60.0);
    ///
    /// let processor = MyProcessor::new();
    /// let processor_id = runtime.add_processor(Box::new(processor));
    /// ```
    pub fn add_processor(&mut self, processor: Box<dyn StreamProcessor>) -> ProcessorId {
        if self.running {
            eprintln!("[Runtime] Warning: Cannot add processor while running");
            return String::new();
        }

        // Generate unique ID
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        // Create shutdown channel for this processor
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);

        // Create processor handle with Pending status
        let handle = ProcessorHandle {
            id: id.clone(),
            name: format!("Processor {}", self.next_processor_id - 1),
            thread: None,
            shutdown_tx,
            status: Arc::new(Mutex::new(ProcessorStatus::Pending)),
            processor: None,  // Will be set when processor is started
        };

        // Add to processor registry
        {
            let mut processors = self.processors.lock().unwrap();
            processors.insert(id.clone(), handle);
        }

        // Add to pending list (will be spawned on start())
        // Store the shutdown_rx so we can use it when spawning the thread
        self.pending_processors.push((id.clone(), processor, shutdown_rx));

        tracing::info!("Added processor with ID: {}", id);
        id
    }

    /// Add a processor to a running runtime
    ///
    /// This method adds and immediately starts a processor while the runtime is running.
    /// Unlike `add_processor()`, this spawns the processor thread immediately.
    ///
    /// # Arguments
    ///
    /// * `processor` - Boxed processor implementation
    ///
    /// # Returns
    ///
    /// * `Ok(ProcessorId)` - The generated ProcessorId for this processor
    /// * `Err(StreamError::Runtime)` - If runtime is not running or GPU context missing
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.start().await?;
    /// // ... later, while running ...
    /// let processor_id = runtime.add_processor_runtime(Box::new(MyProcessor::new())).await?;
    /// ```
    pub async fn add_processor_runtime(
        &mut self,
        mut processor: Box<dyn StreamProcessor>,
    ) -> Result<ProcessorId> {
        // 1. Verify runtime is running
        if !self.running {
            return Err(StreamError::Runtime(
                "Cannot add processor at runtime - runtime is not running. Use add_processor() instead.".into()
            ));
        }

        // 2. Generate unique ID
        let processor_id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        tracing::info!("[{}] Adding processor to running runtime...", processor_id);

        // 3. Create shutdown channel for this processor
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);

        // 4. Get GPU context
        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        // 5. Subscribe processor to broadcaster
        let tick_rx = {
            let mut broadcaster = self.broadcaster.lock().unwrap();
            broadcaster.subscribe()
        };

        // 6. Wrap processor in Arc<Mutex<>> for shared access (enables dynamic connections)
        let processor_arc = Arc::new(Mutex::new(processor));

        // 7. Clone variables for thread
        let id_for_thread = processor_id.clone();
        let processor_gpu_context = gpu_context.clone();
        let processor_for_thread = Arc::clone(&processor_arc);

        // 8. Spawn OS thread for this processor (on_start called within thread)
        let handle = std::thread::spawn(move || {
            tracing::info!("[{}] Thread started", id_for_thread);

            // Call on_start lifecycle hook with GPU context
            {
                let mut processor = processor_for_thread.lock().unwrap();
                if let Err(e) = processor.on_start(&processor_gpu_context) {
                    tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                    return;
                }
            }

            // Process ticks until shutdown signal or channel closes
            loop {
                crossbeam_channel::select! {
                    recv(tick_rx) -> result => {
                        match result {
                            Ok(tick) => {
                                let mut processor = processor_for_thread.lock().unwrap();
                                if let Err(e) = processor.process(tick) {
                                    tracing::error!("[{}] process() error: {}", id_for_thread, e);
                                    // Continue processing (errors are isolated)
                                }
                            }
                            Err(_) => {
                                // Tick channel closed (runtime stopped)
                                tracing::debug!("[{}] Tick channel closed", id_for_thread);
                                break;
                            }
                        }
                    }
                    recv(shutdown_rx) -> result => {
                        // Shutdown signal received
                        match result {
                            Ok(_) | Err(_) => {
                                tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                break;
                            }
                        }
                    }
                }
            }

            // Call on_stop lifecycle hook
            {
                let mut processor = processor_for_thread.lock().unwrap();
                if let Err(e) = processor.on_stop() {
                    tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                }
            }

            tracing::info!("[{}] Thread stopped", id_for_thread);
        });

        // 9. Create and register processor handle (with processor reference for dynamic connections)
        let proc_handle = ProcessorHandle {
            id: processor_id.clone(),
            name: format!("Processor {}", self.next_processor_id - 1),
            thread: Some(handle),
            shutdown_tx,
            status: Arc::new(Mutex::new(ProcessorStatus::Running)),
            processor: Some(processor_arc),  // Store for dynamic connections
        };

        {
            let mut processors = self.processors.lock().unwrap();
            processors.insert(processor_id.clone(), proc_handle);
        }

        tracing::info!("[{}] Processor added to running runtime", processor_id);
        Ok(processor_id)
    }

    pub fn connect<T: crate::core::ports::PortMessage>(
        &mut self,
        output: &mut crate::core::ports::StreamOutput<T>,
        input: &mut crate::core::ports::StreamInput<T>,
    ) -> Result<()> {
        if self.running {
            return Err(StreamError::Configuration(
                "Cannot connect ports while runtime is running".into(),
            ));
        }

        // Perform the actual connection
        input.connect(output.buffer().clone());

        // Register connection in the registry for tracking
        // TODO: Use actual processor IDs and port names when metadata is available
        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        let connection = Connection::new(
            connection_id.clone(),
            "unknown_output".to_string(),  // Will be enhanced in Phase 5
            "unknown_input".to_string(),   // Will be enhanced in Phase 5
        );

        {
            let mut connections = self.connections.lock().unwrap();
            connections.insert(connection_id.clone(), connection);
        }

        tracing::debug!("Registered connection: {}", connection_id);
        Ok(())
    }

    /// Connect two processors at runtime (Phase 5)
    ///
    /// This method connects processors while the runtime is running.
    /// It parses processor IDs and port names from the source/destination strings
    /// (format: "processor_id.port_name"), then connects the specified ports.
    ///
    /// # Arguments
    ///
    /// * `source` - Source port in format "processor_id.port_name" (e.g., "processor_0.video")
    /// * `destination` - Destination port in format "processor_id.port_name"
    ///
    /// # Returns
    ///
    /// * `Ok(ConnectionId)` - The ID of the created connection
    /// * `Err(StreamError)` - If processors not found, ports invalid, or connection fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.connect_at_runtime("processor_0.video", "processor_1.video").await?;
    /// ```
    pub async fn connect_at_runtime(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<ConnectionId> {
        // 1. Parse source and destination (format: "processor_id.port_name")
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

        // 2. Look up both processors
        let (source_processor, dest_processor) = {
            let processors = self.processors.lock().unwrap();

            let source_handle = processors.get(source_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
            })?;

            let dest_handle = processors.get(dest_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Destination processor '{}' not found",
                    dest_proc_id
                ))
            })?;

            // Get processor references (Arc<Mutex<>>)
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

        // 3. Connect the ports
        // For now, this is specialized for CameraProcessor→DisplayProcessor
        // TODO: Generalize this with a trait-based port access system
        {
            // Lock both processors
            let mut source = source_processor.lock().unwrap();
            let mut dest = dest_processor.lock().unwrap();

            // Try to downcast to CameraProcessor and DisplayProcessor
            use crate::core::{CameraProcessor, DisplayProcessor};

            let camera = source
                .as_any_mut()
                .downcast_mut::<crate::CameraProcessor>()
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' is not a CameraProcessor",
                        source_proc_id
                    ))
                })?;

            let display = dest
                .as_any_mut()
                .downcast_mut::<crate::DisplayProcessor>()
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Destination processor '{}' is not a DisplayProcessor",
                        dest_proc_id
                    ))
                })?;

            // Get ports
            let output = &mut camera.output_ports().video;
            let input = &mut display.input_ports().video;

            // Perform connection (same as regular connect())
            input.connect(output.buffer().clone());

            tracing::info!(
                "Connected CameraProcessor {} → DisplayProcessor {}",
                source_proc_id,
                dest_proc_id
            );
        }

        // 4. Register connection in registry
        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        let connection = Connection::new(connection_id.clone(), source.to_string(), destination.to_string());

        {
            let mut connections = self.connections.lock().unwrap();
            connections.insert(connection_id.clone(), connection);
        }

        tracing::info!("Registered runtime connection: {}", connection_id);
        Ok(connection_id)
    }

    /// Start the runtime
    ///
    /// This spawns:
    /// 1. Clock task (tokio) - generates ticks
    /// 2. Handler threads (OS) - process ticks
    ///
    /// After calling start(), the runtime is running and handlers
    /// begin receiving ticks.
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.add_stream(stream1);
    /// runtime.add_stream(stream2);
    /// runtime.start().await?;
    /// ```
    pub async fn start(&mut self) -> Result<()> {
        if self.running {
            return Err(StreamError::Configuration("Runtime already running".into()));
        }

        let handler_count = self.pending_processors.len();

        tracing::info!(
            "Starting runtime with {} processors at {}fps",
            handler_count,
            self.fps
        );

        // Warn if handler count seems high relative to CPU cores
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

        // Initialize GPU context (WebGPU device/queue for all processors)
        tracing::info!("Initializing GPU context...");
        let gpu_context = crate::core::gpu_context::GpuContext::init_for_platform().await?;
        tracing::info!("GPU context initialized: {:?}", gpu_context);
        self.gpu_context = Some(gpu_context);

        self.running = true;

        // 1. Spawn clock task (tokio async)
        self.spawn_clock_task()?;

        // 2. Spawn handler threads (OS threads)
        self.spawn_handler_threads()?;

        tracing::info!("Runtime started successfully");
        Ok(())
    }

    /// Run the runtime until stopped
    ///
    /// This starts the runtime and blocks until `stop()` is called or
    /// the runtime is interrupted (Ctrl+C).
    ///
    /// Automatically handles:
    /// - Starting all processors
    /// - Running the clock
    /// - Platform-specific event loops (if configured)
    /// - Clean shutdown on Ctrl+C
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.run().await?;  // Starts and blocks here
    /// ```
    pub async fn run(&mut self) -> Result<()> {
        // Auto-start if not already running
        if !self.running {
            self.start().await?;
        }

        tracing::info!("Runtime running (press Ctrl+C to stop)");

        // Use platform-specific event loop if provided, otherwise default behavior
        if let Some(event_loop) = self.event_loop.take() {
            // Platform provided an event loop - use it
            tracing::debug!("Using platform-specific event loop");
            event_loop().await?;
        } else {
            // Default behavior: wait for Ctrl+C
            tokio::signal::ctrl_c().await.map_err(|e| {
                StreamError::Configuration(format!("Failed to listen for shutdown signal: {}", e))
            })?;

            tracing::info!("Shutdown signal received");
        }

        self.stop().await?;
        Ok(())
    }

    /// Stop the runtime
    ///
    /// This cleanly shuts down:
    /// 1. Clock task (cancel tokio task)
    /// 2. Handler threads (drop broadcaster to close channels)
    /// 3. Wait for all threads to finish
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.stop().await?;
    /// ```
    pub async fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }

        tracing::info!("Stopping runtime...");
        self.running = false;

        // 1. Cancel clock task
        if let Some(task) = self.clock_task.take() {
            task.abort();
            tracing::debug!("Clock task cancelled");
        }

        // 2. Drop broadcaster to close all channels
        // This makes handler threads' `for tick in rx` loops exit
        {
            let mut broadcaster = self.broadcaster.lock().unwrap();
            broadcaster.clear();
        }
        tracing::debug!("Broadcaster channels closed");

        // 3. Wait for handler threads to finish
        let processor_ids: Vec<ProcessorId> = {
            let processors = self.processors.lock().unwrap();
            processors.keys().cloned().collect()
        };

        let thread_count = processor_ids.len();
        for (i, processor_id) in processor_ids.iter().enumerate() {
            // Take the thread handle from the processor
            let thread_handle = {
                let mut processors = self.processors.lock().unwrap();
                processors
                    .get_mut(processor_id)
                    .and_then(|proc| proc.thread.take())
            };

            if let Some(handle) = thread_handle {
                match handle.join() {
                    Ok(_) => {
                        tracing::debug!("[{}] Thread joined ({}/{})", processor_id, i + 1, thread_count);
                        // Update status to Stopped
                        let mut processors = self.processors.lock().unwrap();
                        if let Some(proc) = processors.get_mut(processor_id) {
                            *proc.status.lock().unwrap() = ProcessorStatus::Stopped;
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

    /// Remove a specific processor from the runtime
    ///
    /// This cleanly shuts down a single processor by:
    /// 1. Sending shutdown signal to the processor thread
    /// 2. Waiting for the thread to join (with timeout)
    /// 3. Updating processor status to Stopped
    ///
    /// # Arguments
    ///
    /// * `processor_id` - The unique ID of the processor to remove
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Processor successfully removed
    /// * `Err(StreamError::NotFound)` - Processor ID not found
    /// * `Err(StreamError::Runtime)` - Processor already stopped or shutdown failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// let processor_id = runtime.add_processor(Box::new(MyProcessor::new()));
    /// runtime.start().await?;
    /// // ... later ...
    /// runtime.remove_processor(&processor_id).await?;
    /// ```
    pub async fn remove_processor(&mut self, processor_id: &ProcessorId) -> Result<()> {
        // 1. Look up processor and verify it exists
        let shutdown_tx = {
            let mut processors = self.processors.lock().unwrap();
            let processor = processors.get_mut(processor_id).ok_or_else(|| {
                StreamError::NotFound(format!("Processor '{}' not found", processor_id))
            })?;

            // Check current status
            let current_status = *processor.status.lock().unwrap();
            if current_status == ProcessorStatus::Stopped || current_status == ProcessorStatus::Stopping {
                return Err(StreamError::Runtime(format!(
                    "Processor '{}' is already {:?}",
                    processor_id, current_status
                )));
            }

            // Update status to Stopping
            *processor.status.lock().unwrap() = ProcessorStatus::Stopping;

            // Clone the shutdown sender
            processor.shutdown_tx.clone()
        };

        tracing::info!("[{}] Removing processor...", processor_id);

        // 2. Send shutdown signal
        shutdown_tx.send(()).map_err(|_| {
            StreamError::Runtime(format!(
                "Failed to send shutdown signal to processor '{}'",
                processor_id
            ))
        })?;

        tracing::debug!("[{}] Shutdown signal sent", processor_id);

        // 3. Wait for thread to join (with timeout)
        let thread_handle = {
            let mut processors = self.processors.lock().unwrap();
            processors
                .get_mut(processor_id)
                .and_then(|proc| proc.thread.take())
        };

        if let Some(handle) = thread_handle {
            // Spawn a task to join the thread
            let join_result = tokio::task::spawn_blocking(move || {
                handle.join()
            }).await;

            match join_result {
                Ok(Ok(_)) => {
                    tracing::info!("[{}] Processor thread joined successfully", processor_id);

                    // 4. Update status to Stopped
                    let mut processors = self.processors.lock().unwrap();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock().unwrap() = ProcessorStatus::Stopped;
                    }
                }
                Ok(Err(panic_err)) => {
                    tracing::error!("[{}] Processor thread panicked: {:?}", processor_id, panic_err);

                    // Still mark as stopped
                    let mut processors = self.processors.lock().unwrap();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock().unwrap() = ProcessorStatus::Stopped;
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

    /// Get runtime status info
    pub fn status(&self) -> RuntimeStatus {
        let handler_count = {
            let processors = self.processors.lock().unwrap();
            processors.len()
        };

        RuntimeStatus {
            running: self.running,
            fps: self.fps,
            handler_count,
            clock_type: if self.clock.is_some() {
                "pending"
            } else {
                "running"
            },
        }
    }

    // Private implementation methods

    fn spawn_clock_task(&mut self) -> Result<()> {
        let mut clock = self
            .clock
            .take()
            .ok_or_else(|| StreamError::Configuration("Clock already started".into()))?;

        let broadcaster = Arc::clone(&self.broadcaster);

        let task = tokio::spawn(async move {
            tracing::info!("[Clock] Started");

            loop {
                // Generate next tick (async wait)
                let tick = clock.next_tick().await;

                // Broadcast to all handlers (non-blocking)
                {
                    let broadcaster = broadcaster.lock().unwrap();
                    broadcaster.broadcast(tick);
                }

                // Note: No explicit sleep - clock.next_tick() handles timing
            }
        });

        self.clock_task = Some(task);
        Ok(())
    }

    fn spawn_handler_threads(&mut self) -> Result<()> {
        // Clone GPU context to pass to all processor threads
        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?
            .clone();

        for (processor_id, mut processor, shutdown_rx) in self.pending_processors.drain(..) {
            // Subscribe processor to broadcaster
            let tick_rx = {
                let mut broadcaster = self.broadcaster.lock().unwrap();
                broadcaster.subscribe()
            };

            // Clone GPU context for this processor thread
            let processor_gpu_context = gpu_context.clone();

            // Clone processor ID for thread logging
            let id_for_thread = processor_id.clone();

            // Spawn OS thread for this processor
            let handle = std::thread::spawn(move || {
                tracing::info!("[{}] Thread started", id_for_thread);

                // Call on_start lifecycle hook with GPU context
                if let Err(e) = processor.on_start(&processor_gpu_context) {
                    tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                    return;
                }

                // Process ticks until shutdown signal or channel closes
                loop {
                    crossbeam_channel::select! {
                        recv(tick_rx) -> result => {
                            match result {
                                Ok(tick) => {
                                    if let Err(e) = processor.process(tick) {
                                        tracing::error!("[{}] process() error: {}", id_for_thread, e);
                                        // Continue processing (errors are isolated)
                                    }
                                }
                                Err(_) => {
                                    // Tick channel closed (runtime stopped)
                                    tracing::debug!("[{}] Tick channel closed", id_for_thread);
                                    break;
                                }
                            }
                        }
                        recv(shutdown_rx) -> result => {
                            // Shutdown signal received
                            match result {
                                Ok(_) | Err(_) => {
                                    tracing::info!("[{}] Shutdown signal received", id_for_thread);
                                    break;
                                }
                            }
                        }
                    }
                }

                // Call on_stop lifecycle hook
                if let Err(e) = processor.on_stop() {
                    tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                }

                tracing::info!("[{}] Thread stopped", id_for_thread);
            });

            // Update processor handle in registry with thread and Running status
            {
                let mut processors = self.processors.lock().unwrap();
                if let Some(proc_handle) = processors.get_mut(&processor_id) {
                    proc_handle.thread = Some(handle);
                    *proc_handle.status.lock().unwrap() = ProcessorStatus::Running;
                } else {
                    tracing::error!("Processor {} not found in registry", processor_id);
                }
            }

            // Also push to handler_threads for backward compatibility
            // TODO: Remove after full migration
            // (Can't push handle here since it was moved to the ProcessorHandle)
        }

        Ok(())
    }
}

/// Runtime status information
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub fps: f64,
    pub handler_count: usize,
    pub clock_type: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clock::TimedTick;
    use crate::core::stream_processor::StreamProcessor;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct CounterProcessor {
        count: Arc<AtomicU64>,
    }

    impl CounterProcessor {
        fn new(count: Arc<AtomicU64>) -> Self {
            Self { count }
        }
    }

    impl StreamProcessor for CounterProcessor {
        fn process(&mut self, _tick: TimedTick) -> Result<()> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn test_runtime_creation() {
        let runtime = StreamRuntime::new(60.0);
        assert!(!runtime.running);
        assert_eq!(runtime.fps, 60.0);
        assert_eq!(runtime.pending_processors.len(), 0);
    }

    #[test]
    fn test_add_processor() {
        let mut runtime = StreamRuntime::new(60.0);

        let count = Arc::new(AtomicU64::new(0));
        let processor = CounterProcessor::new(count);

        runtime.add_processor(Box::new(processor));
        assert_eq!(runtime.pending_processors.len(), 1);
    }

    #[tokio::test]
    async fn test_runtime_lifecycle() {
        let mut runtime = StreamRuntime::new(100.0);

        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));

        runtime.add_processor(Box::new(CounterProcessor::new(Arc::clone(&count1))));
        runtime.add_processor(Box::new(CounterProcessor::new(Arc::clone(&count2))));

        runtime.start().await.unwrap();
        assert!(runtime.running);

        // Check processor registry instead of handler_threads
        {
            let processors = runtime.processors.lock().unwrap();
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
        use std::sync::Mutex as StdMutex;
        use std::time::Instant;

        struct WorkProcessor {
            work_duration_ms: u64,
            start_times: Arc<StdMutex<Vec<Instant>>>,
        }

        impl StreamProcessor for WorkProcessor {
            fn process(&mut self, tick: TimedTick) -> Result<()> {
                self.start_times.lock().unwrap().push(Instant::now());

                let start = Instant::now();
                let mut sum = 0u64;
                while start.elapsed().as_millis() < self.work_duration_ms as u128 {
                    sum = sum.wrapping_add(tick.frame_number);
                }

                if sum == u64::MAX {
                    println!("Never happens");
                }

                Ok(())
            }
        }

        let mut runtime = StreamRuntime::new(10.0);

        let start_times1 = Arc::new(StdMutex::new(Vec::new()));
        let start_times2 = Arc::new(StdMutex::new(Vec::new()));

        runtime.add_processor(Box::new(WorkProcessor {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times1),
        }));

        runtime.add_processor(Box::new(WorkProcessor {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times2),
        }));

        runtime.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(350)).await;

        runtime.stop().await.unwrap();

        let times1 = start_times1.lock().unwrap();
        let times2 = start_times2.lock().unwrap();

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
