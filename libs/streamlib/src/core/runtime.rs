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
use super::handles::{ProcessorHandle, PendingConnection};
use super::{Result, StreamError};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use parking_lot::Mutex;
use std::thread::JoinHandle;

/// Opaque shader ID (for future GPU operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

/// Unique identifier for processors in the runtime
pub type ProcessorId = String;

/// Wakeup event for event-driven processors
///
/// Processors can wake up on different events instead of just global clock ticks.
/// This enables push-based operation where producers wake consumers when data is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupEvent {
    /// Data is available on an input port
    DataAvailable,
    /// Timer tick from global clock
    TimerTick,
    /// Shutdown signal
    Shutdown,
}

/// Global audio configuration for the runtime
///
/// All audio processors (microphone, CLAP plugins, speakers) should use
/// these settings to ensure sample rate compatibility across the pipeline.
#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    /// Sample rate in Hz (e.g., 48000, 44100)
    pub sample_rate: u32,

    /// Number of audio channels (1 = mono, 2 = stereo)
    pub channels: u32,

    /// Buffer size in frames (e.g., 512, 1024, 2048)
    /// Smaller = lower latency, but higher CPU usage
    pub buffer_size: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,  // CD quality+
            channels: 2,         // Stereo
            buffer_size: 2048,   // ~43ms latency at 48kHz
        }
    }
}

/// Clock source for timer group (clock domain)
///
/// Defines what drives the timing for processors in a timer group.
/// Phase 1 uses software timers, future phases can add hardware clocks.
#[derive(Debug, Clone)]
pub enum ClockSource {
    /// Software timer using std::thread::sleep
    ///
    /// Simple, portable, no hardware dependency.
    /// Suitable for processors that don't need hardware-accurate timing.
    Software {
        /// Timer rate in Hz (e.g., 23.44 for audio, 60.0 for video)
        rate_hz: f64,
    },

    // Future: Hardware audio callback, PTP clock, etc.
    // HardwareAudio { driver_processor_id: ProcessorId },
}

/// Timer group (clock domain) - processors that share a timing source
///
/// Multiple processors can join the same timer group to synchronize their
/// wake-ups and eliminate clock drift between independent timers.
///
/// Inspired by:
/// - GStreamer's pipeline clock
/// - Core Audio's clock domains
/// - PipeWire's graph scheduling
/// - JACK's sample-accurate synchronization
struct TimerGroup {
    /// Unique group ID (e.g., "audio_master", "video_60fps")
    id: String,

    /// Clock source driving this group
    clock_source: ClockSource,

    /// Processors in this group
    processor_ids: Vec<ProcessorId>,

    /// Wakeup channels for all processors in group
    /// All channels receive WakeupEvent::TimerTick simultaneously
    wakeup_channels: Vec<crossbeam_channel::Sender<WakeupEvent>>,

    /// Timer thread handle
    timer_thread: Option<std::thread::JoinHandle<()>>,
}

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
/// Type alias for a type-erased processor stored at runtime
///
/// Since processors are constructed before being boxed, we don't need the Config
/// type information in the trait object. We use Box<dyn Any> for type erasure
/// and downcast when needed for port access.
type DynProcessor = Box<dyn super::stream_processor::DynStreamProcessor>;

/// Internal handle for tracking a running processor's state
///
/// This is different from the public-facing `ProcessorHandle` in handles.rs,
/// which is returned by `add_processor()` and used for making connections.
/// This internal handle tracks the processor's thread, shutdown channel, etc.
pub(crate) struct RuntimeProcessorHandle {
    /// Unique processor ID
    pub id: ProcessorId,

    /// Human-readable processor name
    pub name: String,

    /// Processor thread handle (None if not started yet)
    thread: Option<JoinHandle<()>>,

    /// Channel for sending shutdown signal to processor thread
    shutdown_tx: crossbeam_channel::Sender<()>,

    /// Channel for sending wakeup events to processor thread
    pub(crate) wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,

    /// Current processor status
    pub(crate) status: Arc<Mutex<ProcessorStatus>>,

    /// Shared reference to the processor (for dynamic connections at runtime)
    /// Wrapped in Arc<Mutex<>> so it can be accessed from both the processing thread
    /// and connection operations. Uses Any for type erasure since Config is not needed at runtime.
    pub(crate) processor: Option<Arc<Mutex<DynProcessor>>>,
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

    /// Processor registry (maps ID -> RuntimeProcessorHandle)
    /// Tracks all processors (pending, running, stopped)
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, RuntimeProcessorHandle>>>,

    /// Processors waiting to be started (drained on start)
    /// Stores: (processor_id, processor, shutdown_receiver)
    /// Processor is type-erased as DynProcessor since Config type is no longer needed after construction
    /// TODO: Remove after full migration to processor registry
    pending_processors: Vec<(ProcessorId, DynProcessor, crossbeam_channel::Receiver<()>)>,

    /// Handler threads (spawned on start)
    /// TODO: Remove after full migration to processor registry
    #[allow(dead_code)]
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

    /// Global audio configuration
    /// All audio processors should use these settings for sample rate compatibility
    audio_config: AudioConfig,

    /// Pending connections waiting to be wired during start()
    /// Stores connection information before processors are added to runtime
    pending_connections: Vec<PendingConnection>,

    /// Timer groups registry (maps group_id -> TimerGroup)
    /// Processors with same timer group share a master timer thread for synchronization
    timer_groups: Arc<Mutex<HashMap<String, TimerGroup>>>,
}

impl StreamRuntime {
    /// Create a new runtime with software clock
    ///
    /// Defaults to 60 FPS for the internal clock.
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib_core::StreamRuntime;
    ///
    /// let runtime = StreamRuntime::new();
    /// ```
    pub fn new() -> Self {
        let fps = 60.0; // Default FPS
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
            audio_config: AudioConfig::default(),
            pending_connections: Vec::new(),
            timer_groups: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the global audio configuration
    ///
    /// All audio processors should use these settings to ensure
    /// sample rate compatibility across the pipeline.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = runtime.audio_config();
    /// plugin.activate(config.sample_rate, config.buffer_size)?;
    /// ```
    pub fn audio_config(&self) -> AudioConfig {
        self.audio_config
    }

    /// Set the global audio configuration
    ///
    /// **Must be called before starting the runtime**. Changing audio config
    /// after processors are running may cause sample rate mismatches.
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.set_audio_config(AudioConfig {
    ///     sample_rate: 44100,  // 44.1kHz
    ///     channels: 2,
    ///     buffer_size: 1024,   // Lower latency
    /// });
    /// ```
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        if self.running {
            tracing::warn!("Changing audio config while runtime is running may cause issues");
        }
        self.audio_config = config;
    }

    /// Validate that an AudioFrame matches the runtime's audio configuration
    ///
    /// This checks that the frame's sample rate and channel count match the
    /// runtime's global audio config. Use this when processing audio to ensure
    /// pipeline-wide consistency.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Err(e) = runtime.validate_audio_frame(&frame) {
    ///     tracing::warn!("Audio config mismatch: {}", e);
    /// }
    /// ```
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
            audio_config: AudioConfig::default(),
            pending_connections: Vec::new(),
            timer_groups: Arc::new(Mutex::new(HashMap::new())),
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

    /// Add a processor to the runtime using default configuration
    ///
    /// This is the idiomatic Rust way to add processors with default config.
    /// For processors that need configuration, use `add_processor_with_config()`.
    ///
    /// # Type Parameters
    ///
    /// * `P` - The processor type (e.g., TestToneGenerator, AudioMixerProcessor)
    ///
    /// # Returns
    ///
    /// * `Ok(ProcessorHandle)` - Handle to the processor for making connections
    /// * `Err(StreamError)` - If runtime is running or processor construction fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tone = runtime.add_processor::<TestToneGenerator>()?;
    /// let mixer = runtime.add_processor::<AudioMixerProcessor>()?;
    ///
    /// runtime.connect(
    ///     tone.output_port::<AudioFrame>("audio"),
    ///     mixer.input_port::<AudioFrame>("input_1")
    /// )?;
    /// ```
    pub fn add_processor<P: StreamProcessor>(&mut self) -> Result<ProcessorHandle> {
        self.add_processor_with_config::<P>(P::Config::default())
    }

    /// Add a processor to the runtime with custom configuration
    ///
    /// Use this for processors that need configuration (camera device ID, window size, etc.).
    /// For processors with no config needs, use the simpler `add_processor()`.
    ///
    /// # Type Parameters
    ///
    /// * `P` - The processor type (e.g., CameraProcessor, DisplayProcessor)
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for this processor type
    ///
    /// # Returns
    ///
    /// * `Ok(ProcessorHandle)` - Handle to the processor for making connections
    /// * `Err(StreamError)` - If runtime is running or processor construction fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let camera = runtime.add_processor_with_config::<CameraProcessor>(
    ///     CameraConfig { device_id: Some("0x1234".to_string()) }
    /// )?;
    ///
    /// let display = runtime.add_processor_with_config::<DisplayProcessor>(
    ///     DisplayConfig { width: 1920, height: 1080, title: Some("Demo".to_string()) }
    /// )?;
    /// ```
    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<ProcessorHandle> {
        if self.running {
            return Err(StreamError::Runtime(
                "Cannot add processor while runtime is running. Use add_processor_runtime() instead.".into()
            ));
        }

        // Construct processor from config
        let processor = P::from_config(config)?;

        // Generate unique ID
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        // Create shutdown channel for this processor
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);

        // Create dummy wakeup channel (will be replaced in spawn_handler_threads)
        let (dummy_wakeup_tx, _dummy_wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

        // Create internal processor handle with Pending status
        let handle = RuntimeProcessorHandle {
            id: id.clone(),
            name: format!("Processor {}", self.next_processor_id - 1),
            thread: None,
            shutdown_tx,
            wakeup_tx: dummy_wakeup_tx,  // Dummy channel, will be replaced
            status: Arc::new(Mutex::new(ProcessorStatus::Pending)),
            processor: None,  // Will be set when processor is started
        };

        // Add to processor registry
        {
            let mut processors = self.processors.lock();
            processors.insert(id.clone(), handle);
        }

        // Add to pending list (will be spawned on start())
        // Store the shutdown_rx so we can use it when spawning the thread
        // Box as Any for type erasure (Config type no longer needed after construction)
        self.pending_processors.push((id.clone(), Box::new(processor) as DynProcessor, shutdown_rx));

        tracing::info!("Added processor with ID: {}", id);

        // Return public-facing handle for making connections
        Ok(ProcessorHandle::new(id))
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
        processor: Box<dyn super::stream_processor::DynStreamProcessor>,
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

        // 5. Create wakeup channel for this processor (replaces tick broadcaster)
        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

        // 6. Wrap processor in Arc<Mutex<>> for shared access (enables dynamic connections)
        let processor_arc = Arc::new(Mutex::new(processor));

        // Pass wakeup channel to processor
        {
            let mut processor = processor_arc.lock();
            processor.set_wakeup_channel_dyn(wakeup_tx.clone());
        }

        // Check if processor needs timer ticks
        let needs_timer = {
            let processor = processor_arc.lock();
            if let Some(descriptor) = processor.descriptor_instance_dyn() {
                descriptor.timer_requirements.is_some()
            } else {
                false
            }
        };

        // Spawn timer thread if processor requests it
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

        // 7. Clone variables for thread
        let id_for_thread = processor_id.clone();
        let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());
        let processor_for_thread = Arc::clone(&processor_arc);

        // 8. Spawn OS thread for this processor (on_start called within thread)
        let handle = std::thread::spawn(move || {
            tracing::info!("[{}] Thread started", id_for_thread);

            // Call on_start lifecycle hook with runtime context
            {
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.on_start_dyn(&runtime_context) {
                    tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                    return;
                }
            }

            // Process wakeup events until shutdown
            loop {
                crossbeam_channel::select! {
                    recv(wakeup_rx) -> result => {
                        match result {
                            Ok(WakeupEvent::DataAvailable) => {
                                tracing::debug!("[{}] Received DataAvailable wakeup", id_for_thread);
                                let mut processor = processor_for_thread.lock();
                                if let Err(e) = processor.process_dyn() {
                                    tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                    // Continue processing (errors are isolated)
                                }
                            }
                            Ok(WakeupEvent::TimerTick) => {
                                tracing::debug!("[{}] Received TimerTick wakeup", id_for_thread);
                                let mut processor = processor_for_thread.lock();
                                if let Err(e) = processor.process_dyn() {
                                    tracing::error!("[{}] process() error (timer tick): {}", id_for_thread, e);
                                    // Continue processing (errors are isolated)
                                }
                            }
                            Ok(WakeupEvent::Shutdown) => {
                                tracing::info!("[{}] Shutdown wakeup received", id_for_thread);
                                break;
                            }
                            Err(_) => {
                                // Wakeup channel closed
                                tracing::warn!("[{}] Wakeup channel closed unexpectedly", id_for_thread);
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
                let mut processor = processor_for_thread.lock();
                if let Err(e) = processor.on_stop_dyn() {
                    tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                }
            }

            tracing::info!("[{}] Thread stopped", id_for_thread);
        });

        // 9. Create and register processor handle (with processor reference for dynamic connections)
        let proc_handle = RuntimeProcessorHandle {
            id: processor_id.clone(),
            name: format!("Processor {}", self.next_processor_id - 1),
            thread: Some(handle),
            shutdown_tx,
            wakeup_tx,  // Store for connection wiring
            status: Arc::new(Mutex::new(ProcessorStatus::Running)),
            processor: Some(processor_arc),  // Store for dynamic connections
        };

        {
            let mut processors = self.processors.lock();
            processors.insert(processor_id.clone(), proc_handle);
        }

        tracing::info!("[{}] Processor added to running runtime", processor_id);
        Ok(processor_id)
    }

    /// Connect two processors using type-safe port references
    ///
    /// This is the type-safe connection API that should be used in Rust code.
    /// The generic type parameter `T` ensures that only compatible ports can be connected
    /// (e.g., you can't connect AudioFrame to VideoFrame).
    ///
    /// Connections are stored as pending until `start()` is called, at which point
    /// the wakeup channels will be wired up.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The message type (VideoFrame, AudioFrame, etc.)
    ///
    /// # Arguments
    ///
    /// * `output` - Output port reference from source processor
    /// * `input` - Input port reference from destination processor
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Connection registered successfully
    /// * `Err(StreamError)` - If processors not found or ports invalid
    ///
    /// # Example
    ///
    /// ```ignore
    /// let camera = runtime.add_processor::<CameraProcessor>()?;
    /// let display = runtime.add_processor::<DisplayProcessor>()?;
    ///
    /// runtime.connect(
    ///     camera.output_port::<VideoFrame>("video"),
    ///     display.input_port::<VideoFrame>("video")
    /// )?;
    /// ```
    pub fn connect<T: crate::core::ports::PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        // Store pending connection - will be wired during start()
        let pending = PendingConnection::new(
            output.processor_id().clone(),
            output.port_name().to_string(),
            input.processor_id().clone(),
            input.port_name().to_string(),
        );

        self.pending_connections.push(pending.clone());

        // Register connection in the registry for tracking
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

    /// Connect two processors at runtime using string-based port references
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

        // 2.5. Validate audio requirements compatibility (if applicable)
        {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            // Get descriptors using the StreamProcessor trait
            let source_descriptor = source_guard.descriptor_instance_dyn();
            let dest_descriptor = dest_guard.descriptor_instance_dyn();

            // If both processors have audio requirements, validate compatibility
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

        // 3. Wire up the ports using DynStreamProcessor port methods
        {
            let mut source = source_processor.lock();
            let mut dest = dest_processor.lock();

            // Extract consumer from source output port (platform-agnostic)
            let consumer = source
                .take_output_consumer_dyn(source_port)
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' does not have output port '{}'",
                        source_proc_id, source_port
                    ))
                })?;

            // Connect consumer to destination input port (platform-agnostic)
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

        // 4. Register connection in registry
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

    /// Wire up pending connections by transferring rtrb consumers from outputs to inputs
    ///
    /// This connects the actual port ring buffers after processors have been spawned.
    /// Each connection transfers the rtrb::Consumer from the source's StreamOutput
    /// to the destination's StreamInput, enabling lock-free data flow.
    async fn wire_pending_connections(&mut self) -> Result<()> {
        if self.pending_connections.is_empty() {
            tracing::debug!("No pending connections to wire");
            return Ok(());
        }

        tracing::info!("Wiring {} pending connections...", self.pending_connections.len());

        // Drain pending connections and wire them up
        let connections_to_wire = std::mem::take(&mut self.pending_connections);

        for pending in connections_to_wire {
            let source = format!("{}.{}", pending.source_processor_id, pending.source_port_name);
            let destination = format!("{}.{}", pending.dest_processor_id, pending.dest_port_name);

            tracing::info!(
                "Wiring connection: {} → {}",
                source,
                destination
            );

            // Look up both processors
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

                // Get processor references (Arc<Mutex<>>)
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

            // Validate audio requirements compatibility (if applicable)
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

            // Wire up the ports using DynStreamProcessor port methods
            {
                let mut source = source_processor.lock();
                let mut dest = dest_processor.lock();

                // Extract consumer from source output port (platform-agnostic)
                let consumer = source
                    .take_output_consumer_dyn(&pending.source_port_name)
                    .ok_or_else(|| {
                        StreamError::Configuration(format!(
                            "Source processor '{}' does not have output port '{}'",
                            pending.source_processor_id, pending.source_port_name
                        ))
                    })?;

                // Connect consumer to destination input port (platform-agnostic)
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

            // Wire wakeup notifications so output port can wake downstream processor
            {
                let processors = self.processors.lock();

                // Get destination processor's wakeup channel
                if let Some(dest_handle) = processors.get(&pending.dest_processor_id) {
                    let dest_wakeup_tx = dest_handle.wakeup_tx.clone();

                    // Set wakeup channel on source processor's output port
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

    /// Start the runtime
    ///
    /// This spawns:
    /// 1. Clock task (tokio) - generates ticks
    /// 2. Handler threads (OS) - process ticks
    /// 3. Wires up pending connections
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

        // 3. Wire up pending connections now that processors are running
        self.wire_pending_connections().await?;

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
            let mut broadcaster = self.broadcaster.lock();
            broadcaster.clear();
        }
        tracing::debug!("Broadcaster channels closed");

        // 3. Send shutdown signals to all processors
        {
            let processors = self.processors.lock();
            for (processor_id, proc_handle) in processors.iter() {
                if let Err(e) = proc_handle.shutdown_tx.send(()) {
                    tracing::warn!("[{}] Failed to send shutdown signal: {}", processor_id, e);
                }
            }
        }
        tracing::debug!("Shutdown signals sent to all processors");

        // 4. Wait for handler threads to finish
        let processor_ids: Vec<ProcessorId> = {
            let processors = self.processors.lock();
            processors.keys().cloned().collect()
        };

        let thread_count = processor_ids.len();
        for (i, processor_id) in processor_ids.iter().enumerate() {
            // Take the thread handle from the processor
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
                        // Update status to Stopped
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
            let mut processors = self.processors.lock();
            let processor = processors.get_mut(processor_id).ok_or_else(|| {
                StreamError::NotFound(format!("Processor '{}' not found", processor_id))
            })?;

            // Check current status
            let current_status = *processor.status.lock();
            if current_status == ProcessorStatus::Stopped || current_status == ProcessorStatus::Stopping {
                return Err(StreamError::Runtime(format!(
                    "Processor '{}' is already {:?}",
                    processor_id, current_status
                )));
            }

            // Update status to Stopping
            *processor.status.lock() = ProcessorStatus::Stopping;

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
            let mut processors = self.processors.lock();
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
                    let mut processors = self.processors.lock();
                    if let Some(proc) = processors.get_mut(processor_id) {
                        *proc.status.lock() = ProcessorStatus::Stopped;
                    }
                }
                Ok(Err(panic_err)) => {
                    tracing::error!("[{}] Processor thread panicked: {:?}", processor_id, panic_err);

                    // Still mark as stopped
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

    /// Get runtime status info
    pub fn status(&self) -> RuntimeStatus {
        let handler_count = {
            let processors = self.processors.lock();
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
                    let broadcaster = broadcaster.lock();
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

        // Collect timer requirements for grouping
        // Maps group_id -> Vec<(processor_id, rate_hz, wakeup_tx)>
        let mut timer_groups_map: HashMap<String, Vec<(ProcessorId, f64, crossbeam_channel::Sender<WakeupEvent>)>> = HashMap::new();
        // Solo timers (no group_id)
        let mut solo_timers: Vec<(ProcessorId, f64, crossbeam_channel::Sender<WakeupEvent>)> = Vec::new();

        for (processor_id, processor, shutdown_rx) in self.pending_processors.drain(..) {
            // Create wakeup channel for this processor
            // This replaces the tick broadcaster subscription
            let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

            // Wrap processor in Arc<Mutex<>> for shared access (enables dynamic connections)
            let processor_arc = Arc::new(Mutex::new(processor));

            // Pass wakeup channel to processor via set_wakeup_channel
            {
                let mut processor = processor_arc.lock();
                processor.set_wakeup_channel_dyn(wakeup_tx.clone());
            }

            // Collect timer requirements for later grouping
            {
                let processor = processor_arc.lock();
                if let Some(descriptor) = processor.descriptor_instance_dyn() {
                    if let Some(timer_req) = descriptor.timer_requirements {
                        let rate_hz = timer_req.rate_hz;
                        let wakeup_tx_timer = wakeup_tx.clone();

                        if let Some(group_id) = timer_req.group_id {
                            // Join timer group
                            timer_groups_map.entry(group_id)
                                .or_default()
                                .push((processor_id.clone(), rate_hz, wakeup_tx_timer));
                        } else {
                            // Independent timer
                            solo_timers.push((processor_id.clone(), rate_hz, wakeup_tx_timer));
                        }
                    }
                }
            }

            // Create runtime context for this processor thread
            let runtime_context = crate::core::RuntimeContext::new(gpu_context.clone());

            // Clone processor ID for thread logging
            let id_for_thread = processor_id.clone();

            // Clone processor reference for thread
            let processor_for_thread = Arc::clone(&processor_arc);

            // Spawn OS thread for this processor
            let handle = std::thread::spawn(move || {
                tracing::info!("[{}] Thread started", id_for_thread);

                // Call on_start lifecycle hook with runtime context
                {
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.on_start_dyn(&runtime_context) {
                        tracing::error!("[{}] on_start() failed: {}", id_for_thread, e);
                        return;
                    }
                }

                // Process wakeup events until shutdown
                loop {
                    crossbeam_channel::select! {
                        recv(wakeup_rx) -> result => {
                            match result {
                                Ok(WakeupEvent::DataAvailable) => {
                                    let mut processor = processor_for_thread.lock();
                                    if let Err(e) = processor.process_dyn() {
                                        tracing::error!("[{}] process() error (data wakeup): {}", id_for_thread, e);
                                        // Continue processing (errors are isolated)
                                    }
                                }
                                Ok(WakeupEvent::TimerTick) => {
                                    let mut processor = processor_for_thread.lock();
                                    if let Err(e) = processor.process_dyn() {
                                        tracing::error!("[{}] process() error (timer tick): {}", id_for_thread, e);
                                        // Continue processing (errors are isolated)
                                    }
                                }
                                Ok(WakeupEvent::Shutdown) => {
                                    tracing::info!("[{}] Shutdown wakeup received", id_for_thread);
                                    break;
                                }
                                Err(_) => {
                                    // Wakeup channel closed (should not happen normally)
                                    tracing::warn!("[{}] Wakeup channel closed unexpectedly", id_for_thread);
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
                    let mut processor = processor_for_thread.lock();
                    if let Err(e) = processor.on_stop_dyn() {
                        tracing::error!("[{}] on_stop() failed: {}", id_for_thread, e);
                    }
                }

                tracing::info!("[{}] Thread stopped", id_for_thread);
            });

            // Update processor handle in registry with thread, processor reference, wakeup channel, and Running status
            {
                let mut processors = self.processors.lock();
                if let Some(proc_handle) = processors.get_mut(&processor_id) {
                    proc_handle.thread = Some(handle);
                    proc_handle.processor = Some(processor_arc);  // Store for connection wiring
                    proc_handle.wakeup_tx = wakeup_tx;  // Store for connection wiring
                    *proc_handle.status.lock() = ProcessorStatus::Running;
                } else {
                    tracing::error!("Processor {} not found in registry", processor_id);
                }
            }

            // Also push to handler_threads for backward compatibility
            // TODO: Remove after full migration
            // (Can't push handle here since it was moved to the RuntimeProcessorHandle)
        }

        // Spawn timer threads for groups
        for (group_id, members) in timer_groups_map {
            // Validate all members have same rate_hz
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

            // Spawn master timer thread for group
            let group_id_clone = group_id.clone();
            let wakeup_channels_clone = wakeup_channels.clone();
            let timer_thread = std::thread::spawn(move || {
                let interval = std::time::Duration::from_secs_f64(1.0 / rate_hz);
                let mut tick_count = 0u64;

                tracing::info!("[TimerGroup:{}] Timer thread started at {:.2} Hz", group_id_clone, rate_hz);

                loop {
                    std::thread::sleep(interval);
                    tick_count += 1;

                    // Send TimerTick to ALL processors in group simultaneously
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

            // Store timer group
            let mut groups = self.timer_groups.lock();
            groups.insert(group_id.clone(), TimerGroup {
                id: group_id,
                clock_source: ClockSource::Software { rate_hz },
                processor_ids,
                wakeup_channels,
                timer_thread: Some(timer_thread),
            });
        }

        // Spawn individual timers for solo processors (existing behavior)
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
    use crate::core::stream_processor::StreamProcessor;
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
                    rate_hz: 60.0, // Run at 60 Hz
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

        // Check processor registry
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
                        rate_hz: 60.0, // Run at 60 Hz
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

        // Test matching config - should succeed
        let matching_frame = crate::core::AudioFrame::new(
            vec![0.0; 2048],  // 1024 stereo samples
            0,                // timestamp_ns
            0,                // frame_number
            48000,            // sample_rate (matches default)
            2,                // channels (matches default)
        );
        assert!(runtime.validate_audio_frame(&matching_frame).is_ok());

        // Test mismatched sample rate - should fail
        let wrong_sample_rate_frame = crate::core::AudioFrame::new(
            vec![0.0; 2048],
            0,
            0,
            44100,  // Wrong sample rate
            2,
        );
        let result = runtime.validate_audio_frame(&wrong_sample_rate_frame);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sample rate mismatch"));

        // Test mismatched channels - should fail
        let wrong_channels_frame = crate::core::AudioFrame::new(
            vec![0.0; 1024],
            0,
            0,
            48000,
            1,  // Wrong channels (mono instead of stereo)
        );
        let result = runtime.validate_audio_frame(&wrong_channels_frame);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("channel count mismatch"));
    }

    #[test]
    fn test_audio_config_getter_setter() {
        let mut runtime = StreamRuntime::new();

        // Test default config
        let config = runtime.audio_config();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
        assert_eq!(config.buffer_size, 2048);

        // Test custom config
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
