//! Runtime lifecycle management
//!
//! This module contains the core lifecycle methods for the runtime:
//! - `start()` - Initialize and start all processors
//! - `stop()` - Gracefully shut down all processors
//! - `pause()` - Suspend execution (keep state)
//! - `resume()` - Resume from paused state
//! - `restart()` - Stop and start with same graph
//!
//! These methods are implemented as an extension trait on StreamRuntime
//! to keep the lifecycle logic isolated and testable.

use crate::core::error::{Result, StreamError};
use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

use super::state::{ProcessorStatus, RuntimeState};
use super::types::ProcessorId;
use super::StreamRuntime;

impl StreamRuntime {
    /// Start the runtime
    ///
    /// This method:
    /// 1. Validates the runtime is in Stopped state
    /// 2. Initializes the GPU context
    /// 3. Generates the execution plan (if dirty)
    /// 4. Spawns processor threads
    /// 5. Wires pending connections
    /// 6. Transitions to Running state
    ///
    /// # Errors
    /// Returns an error if:
    /// - Runtime is not in Stopped state
    /// - GPU initialization fails
    /// - Thread spawning fails
    /// - Connection wiring fails
    pub fn start(&mut self) -> Result<()> {
        if self.state != RuntimeState::Stopped {
            return Err(StreamError::Configuration(format!(
                "Runtime cannot start from state {:?} (must be Stopped)",
                self.state
            )));
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

        self.state = RuntimeState::Starting;

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

        // Transition to Running state after all initialization complete
        self.state = RuntimeState::Running;

        tracing::info!("Runtime started successfully");
        Ok(())
    }

    /// Run the runtime with event loop
    ///
    /// This method:
    /// 1. Starts the runtime if not already running
    /// 2. Installs signal handlers
    /// 3. Runs the event loop (custom or default)
    /// 4. Stops the runtime when event loop exits
    pub fn run(&mut self) -> Result<()> {
        if self.state != RuntimeState::Running {
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
            self.run_default_event_loop()?;
        }

        self.stop()?;
        Ok(())
    }

    /// Default event loop - waits for shutdown event
    fn run_default_event_loop(&self) -> Result<()> {
        use crate::core::pubsub::{topics, EventListener};
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
        Ok(())
    }

    /// Stop the runtime
    ///
    /// This method:
    /// 1. Publishes shutdown event
    /// 2. Sends shutdown signal to all processor threads
    /// 3. Joins all threads
    /// 4. Transitions to Stopped state
    ///
    /// This method is idempotent - calling it when already stopped is a no-op.
    pub fn stop(&mut self) -> Result<()> {
        if self.state != RuntimeState::Running {
            return Ok(());
        }

        tracing::info!("Stopping runtime...");
        self.state = RuntimeState::Stopping;

        // Publish shutdown event to event bus for shutdown-aware loops
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        EVENT_BUS.publish(&shutdown_event.topic(), &shutdown_event);
        tracing::debug!("Published shutdown event to event bus");

        self.send_shutdown_signals();
        self.join_processor_threads();

        // Transition to Stopped state after all threads joined
        self.state = RuntimeState::Stopped;

        tracing::info!("Runtime stopped");
        Ok(())
    }

    /// Send shutdown signal to all processors
    fn send_shutdown_signals(&self) {
        let processors = self.processors.lock();
        for (processor_id, proc_handle) in processors.iter() {
            if let Err(e) = proc_handle.shutdown_tx.send(()) {
                tracing::warn!("[{}] Failed to send shutdown signal: {}", processor_id, e);
            }
        }
        tracing::debug!("Shutdown signals sent to all processors");
    }

    /// Join all processor threads
    fn join_processor_threads(&mut self) {
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
    }

    /// Pause the runtime (suspend processor threads, keep state)
    ///
    /// # Phase 1 Lifecycle Management
    ///
    /// Pausing allows graph modifications to be made without full shutdown.
    /// While paused:
    /// - Processor threads are suspended (not executing process())
    /// - Graph can be modified (add/remove processors, connect/disconnect)
    /// - State is preserved (no teardown)
    /// - Use `resume()` to continue execution
    ///
    /// # Note
    /// Full thread suspension is platform-specific and not yet implemented.
    /// For now, this just sets the state flag - processors will naturally pause
    /// when they check the runtime state.
    pub fn pause(&mut self) -> Result<()> {
        if self.state != RuntimeState::Running {
            return Err(StreamError::Configuration(format!(
                "Cannot pause from state {:?} (must be Running)",
                self.state
            )));
        }

        tracing::info!("Pausing runtime...");
        self.state = RuntimeState::Paused;

        // TODO(Phase 2+): Implement actual thread suspension
        // For now, processors will naturally pause when they check state
        // This is a graceful pause - processors finish current iteration then wait

        tracing::info!("Runtime paused");
        Ok(())
    }

    /// Resume the runtime from paused state
    ///
    /// If the graph was modified while paused, this will trigger recompilation
    /// before resuming execution.
    pub fn resume(&mut self) -> Result<()> {
        if self.state != RuntimeState::Paused {
            return Err(StreamError::Configuration(format!(
                "Cannot resume from state {:?} (must be Paused)",
                self.state
            )));
        }

        tracing::info!("Resuming runtime...");

        // Recompile if graph changed while paused
        if self.dirty {
            tracing::info!("Graph changed while paused - recompiling...");
            self.recompile()?;
        }

        self.state = RuntimeState::Running;

        // TODO(Phase 2+): Implement actual thread resume
        // For now, processors will naturally resume when they check state

        tracing::info!("Runtime resumed");
        Ok(())
    }

    /// Restart the runtime (stop and start with the same graph)
    ///
    /// This is useful for applying graph changes that require full re-initialization,
    /// or for recovering from errors.
    pub fn restart(&mut self) -> Result<()> {
        if self.state != RuntimeState::Running && self.state != RuntimeState::Paused {
            return Err(StreamError::Configuration(format!(
                "Cannot restart from state {:?} (must be Running or Paused)",
                self.state
            )));
        }

        tracing::info!("Restarting runtime...");
        self.state = RuntimeState::Restarting;

        // Stop all processors
        self.state = RuntimeState::Stopping;
        self.send_shutdown_signals();
        self.join_processor_threads();

        self.state = RuntimeState::Stopped;
        tracing::debug!("Processors stopped for restart");

        // Start again
        self.start()?;

        tracing::info!("Runtime restarted successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pause_requires_running_state() {
        let mut runtime = StreamRuntime::new();

        // Can't pause when Stopped
        assert_eq!(runtime.state(), RuntimeState::Stopped);
        let result = runtime.pause();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be Running"));
    }

    #[test]
    fn test_pause_from_all_invalid_states() {
        let mut runtime = StreamRuntime::new();

        // Test pause from each invalid state
        for state in [
            RuntimeState::Stopped,
            RuntimeState::Starting,
            RuntimeState::Stopping,
            RuntimeState::Paused,
            RuntimeState::Restarting,
            RuntimeState::PurgeRebuild,
        ] {
            runtime.state = state;
            assert!(
                runtime.pause().is_err(),
                "pause() should fail from {:?}",
                state
            );
        }
    }

    #[test]
    fn test_pause_from_running_succeeds() {
        let mut runtime = StreamRuntime::new();
        runtime.state = RuntimeState::Running;

        let result = runtime.pause();
        assert!(result.is_ok());
        assert_eq!(runtime.state(), RuntimeState::Paused);
    }

    #[test]
    fn test_resume_requires_paused_state() {
        let mut runtime = StreamRuntime::new();

        // Can't resume when Stopped
        assert_eq!(runtime.state(), RuntimeState::Stopped);
        let result = runtime.resume();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be Paused"));
    }

    #[test]
    fn test_resume_from_all_invalid_states() {
        let mut runtime = StreamRuntime::new();

        // Test resume from each invalid state
        for state in [
            RuntimeState::Stopped,
            RuntimeState::Starting,
            RuntimeState::Running,
            RuntimeState::Stopping,
            RuntimeState::Restarting,
            RuntimeState::PurgeRebuild,
        ] {
            runtime.state = state;
            assert!(
                runtime.resume().is_err(),
                "resume() should fail from {:?}",
                state
            );
        }
    }

    #[test]
    fn test_resume_from_paused_succeeds() {
        let mut runtime = StreamRuntime::new();
        runtime.state = RuntimeState::Paused;

        let result = runtime.resume();
        assert!(result.is_ok());
        assert_eq!(runtime.state(), RuntimeState::Running);
    }

    #[test]
    fn test_restart_requires_running_or_paused() {
        let mut runtime = StreamRuntime::new();

        // Can't restart when Stopped
        assert_eq!(runtime.state(), RuntimeState::Stopped);
        let result = runtime.restart();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be Running or Paused"));
    }

    #[test]
    fn test_restart_from_all_invalid_states() {
        let mut runtime = StreamRuntime::new();

        // Test restart from each invalid state
        for state in [
            RuntimeState::Stopped,
            RuntimeState::Starting,
            RuntimeState::Stopping,
            RuntimeState::Restarting,
            RuntimeState::PurgeRebuild,
        ] {
            runtime.state = state;
            assert!(
                runtime.restart().is_err(),
                "restart() should fail from {:?}",
                state
            );
        }
    }

    #[test]
    fn test_stop_idempotent_when_not_running() {
        let mut runtime = StreamRuntime::new();

        // Stop when already stopped - should be no-op
        runtime.state = RuntimeState::Stopped;
        let result = runtime.stop();
        assert!(result.is_ok());
        assert_eq!(runtime.state(), RuntimeState::Stopped);

        // Stop when paused - should be no-op (stop requires Running)
        runtime.state = RuntimeState::Paused;
        let result = runtime.stop();
        assert!(result.is_ok());
        assert_eq!(runtime.state(), RuntimeState::Paused);
    }

    #[test]
    fn test_start_requires_stopped_state() {
        let mut runtime = StreamRuntime::new();

        // Can start from Stopped
        assert_eq!(runtime.state(), RuntimeState::Stopped);

        // Manually set to Running - can't start
        runtime.state = RuntimeState::Running;
        let result = runtime.start();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be Stopped"));
    }

    #[test]
    fn test_error_messages_include_state() {
        let mut runtime = StreamRuntime::new();

        // Test that error messages are descriptive
        runtime.state = RuntimeState::Stopped;
        let err = runtime.pause().unwrap_err();
        assert!(err.to_string().contains("Stopped"));
        assert!(err.to_string().contains("Running"));

        runtime.state = RuntimeState::Running;
        let err = runtime.resume().unwrap_err();
        assert!(err.to_string().contains("Running"));
        assert!(err.to_string().contains("Paused"));

        runtime.state = RuntimeState::Stopped;
        let err = runtime.restart().unwrap_err();
        assert!(err.to_string().contains("Stopped"));
    }
}
