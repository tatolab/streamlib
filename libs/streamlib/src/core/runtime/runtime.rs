// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::Serialize;

use super::RuntimeStatus;
use crate::core::compiler::{shutdown_all_processors, Compiler, PendingOperation};
use crate::core::context::{GpuContext, RuntimeContext};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, GraphState,
    LinkOutputToProcessorWriterAndReader, LinkUniqueId, PendingDeletionComponent,
    ProcessorPauseGateComponent, ProcessorUniqueId,
};
use crate::core::links::LinkOutputToProcessorMessage;
use crate::core::processors::{ProcessorSpec, ProcessorState};
use crate::core::pubsub::{topics, Event, EventListener, ProcessorEvent, RuntimeEvent, PUBSUB};
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, StreamError};

/// The main stream processing runtime.
///
/// # Thread Safety
///
/// `StreamRuntime` is designed for concurrent access from multiple threads.
/// All public methods take `&self` (not `&mut self`), allowing the runtime
/// to be shared via `Arc<StreamRuntime>` without external synchronization.
///
/// Internal state uses fine-grained locking:
/// - Graph operations: `RwLock` (multiple readers OR one writer)
/// - Pending operations: `Mutex` (batched for compilation)
/// - Status: `Mutex` (lifecycle state)
/// - Runtime context: `OnceLock` (set once at start, read thereafter)
///
/// This means multiple threads can concurrently call `add_processor()`,
/// `connect()`, etc. without blocking each other on an outer lock.
pub struct StreamRuntime {
    /// Compiles graph changes into running processors. Owns the graph and transaction.
    pub(crate) compiler: Compiler,
    /// Runtime context (GPU, audio config). Set once during start().
    pub(crate) runtime_context: OnceLock<Arc<RuntimeContext>>,
    /// Runtime lifecycle status. Protected by Mutex for interior mutability.
    pub(crate) status: Mutex<RuntimeStatus>,
}

impl StreamRuntime {
    pub fn new() -> Result<Self> {
        // Register all processors from inventory before any add_processor calls.
        // This populates the global registry with link-time registered processors.
        let result = crate::core::processors::PROCESSOR_REGISTRY.register_all_processors()?;
        tracing::debug!("Registered {} processors from inventory", result.count);

        Ok(Self {
            compiler: Compiler::new(),
            runtime_context: OnceLock::new(),
            status: Mutex::new(RuntimeStatus::Initial),
        })
    }

    // =========================================================================
    // Commit Control
    // =========================================================================

    /// Apply all pending graph changes.
    ///
    /// When the runtime is not started, pending operations are kept in the queue
    /// and will be executed when `start()` is called. When started, operations
    /// are compiled and executed with proper processor lifecycle.
    ///
    /// All pending operations are batched into a single compilation pass.
    /// This ensures processors are wired BEFORE their threads start,
    /// avoiding deadlocks when wiring tries to lock a running processor.
    pub fn commit(&self) -> Result<()> {
        // Only compile when runtime is started
        if *self.status.lock() != RuntimeStatus::Started {
            tracing::info!("[commit] Runtime not started, operations queued, operations will be performed after starting runtime. If this is unexpected please call runtime.start(). On start a commit will automatically be submitted for you.");
            return Ok(());
        } else {
            tracing::info!("[commit] Runtime started, commit operations to compiler");
        }

        // Runtime is started - delegate to compiler
        let runtime_ctx = self
            .runtime_context
            .get()
            .ok_or_else(|| {
                crate::core::error::StreamError::Runtime(
                    "Runtime context not initialized".to_string(),
                )
            })?
            .clone();

        self.compiler.commit(&runtime_ctx)
    }

    // =========================================================================
    // Graph Mutations
    // =========================================================================

    /// Add a processor to the graph with its spec. Returns the processor ID.
    pub fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        // Declare side effects upfront
        let emit_will_add = |id: &ProcessorUniqueId| {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillAddProcessor {
                    processor_id: id.clone(),
                }),
            );
        };

        let emit_did_add = |id: &ProcessorUniqueId| {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidAddProcessor {
                    processor_id: id.clone(),
                }),
            );
        };

        // Use compiler.scope() to access graph and transaction
        let processor_id = self.compiler.scope(|graph, tx| {
            graph
                .traversal_mut()
                .add_v(spec)
                .inspect(|node| emit_will_add(&node.id))
                .inspect(|node| tx.log(PendingOperation::AddProcessor(node.id.clone())))
                .inspect(|node| emit_did_add(&node.id))
                .first()
                .map(|node| node.id.clone())
                .ok_or_else(|| StreamError::GraphError("Could not create node".into()))
        })?;

        // Commit changes
        self.commit()?;

        Ok(processor_id)
    }

    /// Connect two ports - adds a link to the graph. Returns the link ID.
    pub fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        // Capture for events before moving into add_e
        let from_processor = from.processor_id.clone();
        let from_port = from.port_name.clone();
        let to_processor = to.processor_id.clone();
        let to_port = to.port_name.clone();

        // Emit WillConnect before the action
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
                from_processor,
                from_port: from_port.clone(),
                to_processor,
                to_port: to_port.clone(),
            }),
        );

        // Use compiler.scope() to access graph and transaction
        let link_id = self.compiler.scope(|graph, tx| {
            let id = graph
                .traversal_mut()
                .add_e(from, to)
                .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
                .first()
                .map(|link| link.id.clone())
                .ok_or_else(|| StreamError::GraphError("failed to create link".into()))?;

            Ok::<_, StreamError>(id)
        })?;

        // Emit DidConnect after the action
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidConnect {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            }),
        );

        // Commit changes
        self.commit()?;

        Ok(link_id)
    }

    pub fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        // Validate link exists and get info for events, then mark for deletion
        let link_info = self.compiler.scope(|graph, tx| {
            let (from_value, to_value) = graph
                .traversal()
                .e(link_id)
                .first()
                .map(|l| (l.from_port(), l.to_port()))
                .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", link_id)))?;

            let info = (
                OutputLinkPortRef::new(from_value.processor_id.clone(), to_value.port_name.clone()),
                InputLinkPortRef::new(to_value.processor_id.clone(), to_value.port_name.clone()),
            );

            // Mark for soft-delete by adding PendingDeletion component to link
            if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
                link.insert(PendingDeletionComponent);
            }

            // Queue operation for commit
            tx.log(PendingOperation::RemoveLink(link_id.clone()));

            Ok::<_, StreamError>(info)
        })?;

        // Emit WillDisconnect before the action
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillDisconnect {
                link_id: link_id.to_string(),
                from_port: link_info.0.to_string(),
                to_port: link_info.1.to_string(),
            }),
        );

        // Emit DidDisconnect after queueing (actual removal happens at commit)
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidDisconnect {
                link_id: link_id.to_string(),
                from_port: link_info.0.to_string(),
                to_port: link_info.1.to_string(),
            }),
        );

        // Commit changes
        self.commit()
    }

    pub fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        // Validate processor exists and mark for deletion
        self.compiler.scope(|graph, tx| {
            if !graph.traversal().v(processor_id).exists() {
                return Err(StreamError::ProcessorNotFound(processor_id.to_string()));
            }

            // Mark for soft-delete by adding PendingDeletion component
            if let Some(node) = graph.traversal_mut().v(processor_id).first_mut() {
                node.insert(PendingDeletionComponent);
            }

            // Queue operation for commit
            tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));

            Ok(())
        })?;

        // Emit WillRemoveProcessor before the action
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillRemoveProcessor {
                processor_id: processor_id.clone(),
            }),
        );

        // Emit DidRemoveProcessor after queueing (actual removal happens at commit)
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRemoveProcessor {
                processor_id: processor_id.clone(),
            }),
        );

        // Commit changes
        self.commit()
    }

    /// Update a processor's configuration at runtime.
    pub fn update_processor_config<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        let config_json = serde_json::to_value(&config)
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;

        // Update config in graph and queue operation
        self.compiler.scope(|graph, tx| {
            if let Some(processor) = graph.traversal_mut().v(processor_id).first_mut() {
                processor.set_config(config_json);
            }

            tx.log(PendingOperation::UpdateProcessorConfig(
                processor_id.clone(),
            ));
        });

        // Publish event
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorConfigDidChange {
                processor_id: processor_id.clone(),
            }),
        );

        // Commit changes
        self.commit()
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the runtime.
    pub fn start(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Starting;
        tracing::info!("[start] Starting runtime");
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        // Initialize GPU context FIRST, before any macOS app setup.
        // wgpu's Metal backend uses async operations that need to complete
        // before NSApplication configuration changes thread behavior.
        if self.runtime_context.get().is_none() {
            tracing::info!("[start] Initializing GPU context...");
            let gpu = GpuContext::init_for_platform_sync()?;
            tracing::info!("[start] GPU context initialized");
            // OnceLock::set returns Err if already set, which is fine - we checked above
            let _ = self.runtime_context.set(Arc::new(RuntimeContext::new(gpu)));
        }

        // macOS standalone app setup: detect if we're running as a standalone app
        // (not embedded in another app with its own NSApplication event loop)
        #[cfg(target_os = "macos")]
        {
            use objc2::MainThreadMarker;
            use objc2_app_kit::NSApplication;

            let is_standalone = if let Some(mtm) = MainThreadMarker::new() {
                let app = NSApplication::sharedApplication(mtm);
                !app.isRunning()
            } else {
                // Not on main thread - can't check NSApplication state
                false
            };

            if is_standalone {
                tracing::info!("[start] Setting up macOS application");
                crate::apple::runtime_ext::setup_macos_app();
                crate::apple::runtime_ext::install_macos_shutdown_handler();

                // CRITICAL: Verify the macOS platform is fully ready BEFORE starting
                // any processors. This uses Apple's NSRunningApplication.isFinishedLaunching
                // API to confirm the app has completed its launch sequence.
                //
                // Without this verification, processors may try to use AVFoundation,
                // Metal, or other Apple frameworks before NSApplication is ready,
                // causing hangs or undefined behavior.
                tracing::info!("[start] Verifying macOS platform readiness...");
                crate::apple::runtime_ext::ensure_macos_platform_ready()?;
            }
        }

        // Set graph state to Running
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        // Mark runtime as started so commit() will actually compile
        *self.status.lock() = RuntimeStatus::Started;

        // Compile any pending changes (includes Phase 4: START)
        // This is now safe because we've verified the platform is ready above.
        self.commit()?;

        tracing::info!("[start] Runtime started (platform verified)");
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
        );

        Ok(())
    }

    /// Stop the runtime.
    pub fn stop(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Stopping;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping),
        );

        // Shutdown all processors
        self.compiler
            .scope(|graph, _tx| shutdown_all_processors(graph))?;

        *self.status.lock() = RuntimeStatus::Stopped;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
        );

        Ok(())
    }

    // =========================================================================
    // Per-Processor Pause/Resume
    // =========================================================================

    /// Pause a specific processor.
    pub fn pause_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.compiler.scope(|graph, _tx| {
            // Validate processor exists
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            // Check if already paused
            if pause_gate.is_paused() {
                return Ok(()); // Already paused, no-op
            }

            // Set the pause gate
            pause_gate
                .clone_inner()
                .store(true, std::sync::atomic::Ordering::Release);

            // Update processor state
            if let Some(state) = node.get::<crate::core::graph::StateComponent>() {
                *state.0.lock() = ProcessorState::Paused;
            }

            // Publish event
            let event = Event::processor(processor_id, ProcessorEvent::Paused);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor paused", processor_id);
            Ok(())
        })
    }

    /// Resume a specific processor.
    pub fn resume_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.compiler.scope(|graph, _tx| {
            // Validate processor exists
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            // Check if already running
            if !pause_gate.is_paused() {
                return Ok(()); // Already running, no-op
            }

            // Clear the pause gate
            pause_gate
                .clone_inner()
                .store(false, std::sync::atomic::Ordering::Release);

            // Update processor state
            if let Some(state) = node.get::<crate::core::graph::StateComponent>() {
                *state.0.lock() = ProcessorState::Running;
            }

            // Send a wake-up message to reactive processors so they can process
            // any buffered data. Without this, a reactive processor could stay
            // blocked if its upstream buffer was full during pause (no new
            // InvokeProcessingNow messages would be sent since writes fail).
            if let Some(channel) = node.get::<LinkOutputToProcessorWriterAndReader>() {
                let _ = channel
                    .writer
                    .send(LinkOutputToProcessorMessage::InvokeProcessingNow);
            }

            // Publish event
            let event = Event::processor(processor_id, ProcessorEvent::Resumed);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor resumed", processor_id);
            Ok(())
        })
    }

    /// Check if a specific processor is paused.
    pub fn is_processor_paused(&self, processor_id: &ProcessorUniqueId) -> Result<bool> {
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node
                .get::<ProcessorPauseGateComponent>()
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            Ok(pause_gate.is_paused())
        })
    }

    // =========================================================================
    // Runtime-level Pause/Resume (all processors)
    // =========================================================================

    /// Pause the runtime (all processors).
    pub fn pause(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Pausing;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePausing),
        );

        // Get all processor IDs
        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        // Pause each processor
        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.pause_processor(processor_id) {
                tracing::warn!("[{}] Failed to pause: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        // Set graph state to Paused
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Paused);
        });

        *self.status.lock() = RuntimeStatus::Paused;
        if failures.is_empty() {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimePaused),
            );
        } else {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimePauseFailed {
                    error: format!("{} processor(s) rejected pause", failures.len()),
                }),
            );
        }

        Ok(())
    }

    /// Resume the runtime (all processors).
    pub fn resume(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Starting;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResuming),
        );

        // Get all processor IDs
        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        // Resume each processor
        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.resume_processor(processor_id) {
                tracing::warn!("[{}] Failed to resume: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        // Set graph state to Running
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        *self.status.lock() = RuntimeStatus::Started;
        if failures.is_empty() {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumed),
            );
        } else {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumeFailed {
                    error: format!("{} processor(s) rejected resume", failures.len()),
                }),
            );
        }

        Ok(())
    }

    /// Block until shutdown signal (Ctrl+C, SIGTERM, Cmd+Q).
    pub fn wait_for_signal(&self) -> Result<()> {
        self.wait_for_signal_with(|_| ControlFlow::Continue(()))
    }

    /// Block until shutdown signal, with periodic callback for dynamic control.
    #[allow(unused_variables, unused_mut)]
    pub fn wait_for_signal_with<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(&Self) -> ControlFlow<()>,
    {
        // Install signal handlers
        crate::core::signals::install_signal_handlers().map_err(|e| {
            crate::core::StreamError::Configuration(format!(
                "Failed to install signal handlers: {}",
                e
            ))
        })?;

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = Arc::clone(&shutdown_flag);

        // Listener that sets shutdown flag when RuntimeShutdown received
        struct ShutdownListener {
            flag: Arc<AtomicBool>,
        }

        impl EventListener for ShutdownListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) = event {
                    self.flag.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }

        let listener = ShutdownListener {
            flag: shutdown_flag_clone.clone(),
        };
        PUBSUB.subscribe(
            topics::RUNTIME_GLOBAL,
            Arc::new(parking_lot::Mutex::new(listener)),
        );

        // On macOS, run the NSApplication event loop (required for GUI)
        #[cfg(target_os = "macos")]
        {
            crate::apple::runtime_ext::run_macos_event_loop();
            // Event loop exited (Cmd+Q or terminate)
            self.stop()?;
            Ok(())
        }

        // Non-macOS: poll loop
        #[cfg(not(target_os = "macos"))]
        {
            while !shutdown_flag.load(Ordering::SeqCst) {
                // Call user callback
                if let ControlFlow::Break(()) = callback(self) {
                    break;
                }

                // Small sleep to avoid busy-waiting
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // Auto-stop on exit
            self.stop()?;

            Ok(())
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        self.compiler.scope(|graph, _tx| {
            serde_json::to_value(&*graph)
                .map_err(|_| StreamError::GraphError("Unable to serialize graph".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let _runtime = StreamRuntime::new();
        // Runtime creates successfully
    }
}
