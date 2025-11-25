//! Runtime module for stream processing
//!
//! This module provides the core runtime for managing processor lifecycles,
//! connections, and execution scheduling.
//!
//! # Module Organization
//!
//! The runtime is organized into several submodules:
//!
//! - [`state`] - Runtime state machine types (RuntimeState, ProcessorStatus, WakeupEvent)
//! - [`types`] - Data structures (Connection, RuntimeProcessorHandle, type aliases)
//! - [`lifecycle`] - Lifecycle methods (start, stop, pause, resume, restart)
//! - [`connections`] - Connection management (connect, disconnect, wiring)
//!
//! # Example
//!
//! ```rust,ignore
//! use streamlib::StreamRuntime;
//!
//! let mut runtime = StreamRuntime::new();
//!
//! // Add processors
//! let camera = runtime.add_processor::<CameraProcessor>()?;
//! let display = runtime.add_processor::<DisplayProcessor>()?;
//!
//! // Connect them
//! runtime.connect(camera.output("video"), display.input("video"))?;
//!
//! // Run the pipeline
//! runtime.run()?;
//! ```

pub mod connections;
pub mod delta;
pub mod lifecycle;
pub mod state;
pub mod types;

// Re-export primary types
pub use delta::{compute_delta, ExecutionDelta};
pub use state::{ProcessorStatus, RuntimeState, WakeupEvent};
pub use types::{Connection, ConnectionId, EventLoopFn, ProcessorId, RuntimeStatus, ShaderId};

// Internal use only - not part of public API
pub(crate) use types::{DynProcessor, RuntimeProcessorHandle};

use crate::core::handles::{PendingConnection, ProcessorHandle};
use crate::core::traits::StreamProcessor;
use crate::core::{Result, StreamError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

/// The main stream processing runtime
///
/// `StreamRuntime` manages the lifecycle of processors and their connections.
/// It handles:
/// - Processor registration and lifecycle management
/// - Connection creation and wiring
/// - Thread spawning and scheduling
/// - GPU context initialization
/// - Graph optimization and execution planning
///
/// # State Machine
///
/// The runtime operates as a state machine with the following states:
///
/// ```text
/// ┌─────────┐
/// │ Stopped │◄──────────────────────────┐
/// └────┬────┘                           │
///      │ start()                        │
///      ▼                                │
/// ┌──────────┐                          │
/// │ Starting │                          │
/// └────┬─────┘                          │
///      │ initialization complete        │
///      ▼                                │
/// ┌─────────┐  pause()   ┌────────┐     │
/// │ Running │───────────►│ Paused │     │
/// └────┬────┘◄───────────┴────────┘     │
///      │      resume()                  │
///      │                                │
///      │ stop()                         │
///      ▼                                │
/// ┌──────────┐                          │
/// │ Stopping │──────────────────────────┘
/// └──────────┘
/// ```
pub struct StreamRuntime {
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, RuntimeProcessorHandle>>>,
    pub(super) pending_processors:
        Vec<(ProcessorId, DynProcessor, crossbeam_channel::Receiver<()>)>,
    #[allow(dead_code)]
    handler_threads: Vec<JoinHandle<()>>,
    /// Runtime state machine
    pub(super) state: RuntimeState,
    pub(super) event_loop: Option<EventLoopFn>,
    pub(super) gpu_context: Option<crate::core::context::GpuContext>,
    pub(super) next_processor_id: usize,
    pub(crate) connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,
    pub(super) next_connection_id: usize,
    pub(super) pending_connections: Vec<PendingConnection>,
    pub(super) bus: crate::core::Bus,
    /// Index for fast connection lookup by processor ID
    pub(super) processor_connections: HashMap<ProcessorId, Vec<ConnectionId>>,
    /// Graph representation (source of truth for desired topology)
    pub(super) graph: crate::core::graph::Graph,
    /// Graph optimizer for topology analysis and execution plan generation
    pub(super) graph_optimizer: crate::core::graph_optimizer::GraphOptimizer,
    /// Current execution plan
    pub(super) execution_plan: Option<crate::core::graph_optimizer::ExecutionPlan>,
    /// Graph has changed, needs recompilation
    pub(super) dirty: bool,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    /// Create a new runtime instance
    pub fn new() -> Self {
        Self {
            processors: Arc::new(Mutex::new(HashMap::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            state: RuntimeState::Stopped,
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

    /// Check if runtime is currently running (threads active)
    pub fn is_running(&self) -> bool {
        self.state == RuntimeState::Running
    }

    /// Get current runtime state
    pub fn state(&self) -> RuntimeState {
        self.state
    }

    /// Check if graph has pending changes that need recompilation
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Auto-recompile based on runtime state
    ///
    /// Called automatically after graph mutations (add_processor, connect, etc.).
    /// - **Running/Paused**: Recompile immediately (hot reloading)
    /// - **Stopped/Starting/Stopping**: Defer until start()
    /// - **Restarting/PurgeRebuild**: In transition, defer
    pub(super) fn try_auto_recompile(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        match self.state {
            RuntimeState::Running | RuntimeState::Paused => {
                tracing::debug!("Auto-recompiling (runtime {:?})...", self.state);
                self.recompile()?;
            }
            RuntimeState::Stopped | RuntimeState::Stopping | RuntimeState::Starting => {
                tracing::debug!(
                    "Deferring recompile (runtime {:?}) - will recompile at start()",
                    self.state
                );
            }
            RuntimeState::Restarting | RuntimeState::PurgeRebuild => {
                tracing::debug!(
                    "Deferring recompile (runtime {:?}) - in transition",
                    self.state
                );
            }
        }
        Ok(())
    }

    /// Recompile the execution plan from the current graph
    pub(super) fn recompile(&mut self) -> Result<()> {
        tracing::info!("Recompiling execution plan...");

        self.graph.validate()?;

        let plan = self.graph_optimizer.optimize(&self.graph)?;
        tracing::debug!("New execution plan: {:?}", plan);

        self.execution_plan = Some(plan);
        self.dirty = false;

        tracing::info!("Execution plan recompiled successfully");
        Ok(())
    }

    /// Request camera permission from the system
    #[cfg(target_os = "macos")]
    pub fn request_camera(&self) -> Result<bool> {
        crate::request_camera_permission()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn request_camera(&self) -> Result<bool> {
        Ok(true)
    }

    /// Request microphone permission from the system
    #[cfg(target_os = "macos")]
    pub fn request_microphone(&self) -> Result<bool> {
        crate::request_audio_permission()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn request_microphone(&self) -> Result<bool> {
        Ok(true)
    }

    /// Set a custom event loop function
    pub fn set_event_loop(&mut self, event_loop: EventLoopFn) {
        self.event_loop = Some(event_loop);
    }

    /// Get reference to GPU context
    pub fn gpu_context(&self) -> Option<&crate::core::context::GpuContext> {
        self.gpu_context.as_ref()
    }

    /// Get reference to graph (for testing/debugging)
    pub fn graph(&self) -> &crate::core::graph::Graph {
        &self.graph
    }

    /// Get reference to current execution plan
    pub fn execution_plan(&self) -> Option<&crate::core::graph_optimizer::ExecutionPlan> {
        self.execution_plan.as_ref()
    }

    /// Get reference to graph optimizer
    pub fn graph_optimizer(&self) -> &crate::core::graph_optimizer::GraphOptimizer {
        &self.graph_optimizer
    }

    /// Get mutable reference to graph optimizer
    pub fn graph_optimizer_mut(&mut self) -> &mut crate::core::graph_optimizer::GraphOptimizer {
        &mut self.graph_optimizer
    }

    /// Add a processor with default configuration
    pub fn add_processor<P: StreamProcessor>(&mut self) -> Result<ProcessorHandle> {
        self.add_processor_with_config::<P>(P::Config::default())
    }

    /// Add a processor with custom configuration
    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<ProcessorHandle> {
        let processor = P::from_config(config)?;
        let processor_dyn: DynProcessor = Box::new(processor);
        self.add_boxed_processor(processor_dyn)
    }

    /// Add a boxed processor (for dynamic processor creation)
    pub fn add_boxed_processor(&mut self, processor: DynProcessor) -> Result<ProcessorHandle> {
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;

        let processor_type = std::any::type_name_of_val(&*processor).to_string();

        if self.state == RuntimeState::Running {
            self.spawn_processor_thread(id.clone(), processor)?;
            tracing::info!(
                "Added processor with ID: {} (runtime running, thread spawned)",
                id
            );
        } else {
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

        // Update graph and execution plan
        self.graph
            .add_processor(id.clone(), processor_type.clone(), 0);

        // Publish event
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorAdded {
                processor_id: id.clone(),
                processor_type,
            }),
        );
        self.dirty = true;
        self.try_auto_recompile()?;

        Ok(ProcessorHandle::new(id))
    }

    /// Spawn a processor thread (internal)
    pub(super) fn spawn_processor_thread(
        &mut self,
        id: ProcessorId,
        processor: DynProcessor,
    ) -> Result<()> {
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<WakeupEvent>();

        let status = Arc::new(Mutex::new(ProcessorStatus::Running));
        let processor_arc = Arc::new(Mutex::new(processor));

        // Setup processor
        {
            let mut guard = processor_arc.lock();
            let gpu_context = self
                .gpu_context
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("GPU context not initialized".to_string()))?;
            let ctx = crate::core::context::RuntimeContext::new(gpu_context.clone());
            guard.__generated_setup(&ctx)?;
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

        let handle = RuntimeProcessorHandle {
            id: id.clone(),
            name: format!("Processor {}", id),
            thread: Some(thread),
            shutdown_tx,
            wakeup_tx,
            status,
            processor: Some(processor_arc),
        };

        {
            let mut processors = self.processors.lock();
            processors.insert(id, handle);
        }

        Ok(())
    }

    /// Processor thread main loop
    fn processor_thread_loop(
        id: ProcessorId,
        processor: Arc<Mutex<DynProcessor>>,
        shutdown_rx: crossbeam_channel::Receiver<()>,
        wakeup_rx: crossbeam_channel::Receiver<WakeupEvent>,
        status: Arc<Mutex<ProcessorStatus>>,
        sched_config: crate::core::scheduling::SchedulingConfig,
    ) {
        use crate::core::scheduling::SchedulingMode;

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
            guard.__generated_teardown();
        }

        *status.lock() = ProcessorStatus::Stopped;
        tracing::debug!("[{}] Thread stopped", id);
    }

    /// Loop scheduling mode
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

    /// Push scheduling mode
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

    /// Pull scheduling mode
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

    /// Spawn handler threads at runtime start
    pub(super) fn spawn_handler_threads(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.pending_processors);

        for (id, processor, _shutdown_rx) in pending {
            self.spawn_processor_thread(id, processor)?;
        }

        Ok(())
    }

    /// Remove a processor from the runtime
    ///
    /// This method:
    /// 1. Sends shutdown signal to the processor thread
    /// 2. Waits for the thread to join
    /// 3. Publishes ProcessorRemoved event
    /// 4. Updates the graph
    /// 5. Cleans up connection index
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

    /// Get connections for a specific processor
    pub fn get_connections_for_processor(&self, processor_id: &ProcessorId) -> Vec<ConnectionId> {
        self.processor_connections
            .get(processor_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get runtime status snapshot
    pub fn status(&self) -> RuntimeStatus {
        let processors = self.processors.lock();
        let connections = self.connections.lock();

        RuntimeStatus {
            running: self.state == RuntimeState::Running,
            processor_count: processors.len(),
            connection_count: connections.len(),
            processor_statuses: processors
                .iter()
                .map(|(id, handle)| (id.clone(), *handle.status.lock()))
                .collect(),
        }
    }
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
        assert_eq!(runtime.state, RuntimeState::Stopped);
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
    fn test_state_accessors() {
        let runtime = StreamRuntime::new();

        assert_eq!(runtime.state(), RuntimeState::Stopped);
        assert!(!runtime.is_running());
        assert!(!runtime.is_dirty());
    }

    #[test]
    fn test_processor_handle_id_format() {
        let mut runtime = StreamRuntime::new();

        let handle1 = runtime.add_processor::<CounterProcessor>().unwrap();
        let handle2 = runtime.add_processor::<CounterProcessor>().unwrap();

        assert_eq!(handle1.id(), "processor_0");
        assert_eq!(handle2.id(), "processor_1");
    }
}
