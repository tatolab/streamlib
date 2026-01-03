// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::GpuContext;
use crate::core::graph::ProcessorUniqueId;
use crate::core::runtime::{RuntimeOperations, RuntimeUniqueId};

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
    /// Unique identifier for this runtime instance.
    runtime_id: Arc<RuntimeUniqueId>,
    /// Unique identifier for this processor (None for shared/global context).
    processor_id: Option<ProcessorUniqueId>,
    /// Pause gate for this processor (None for shared/global context).
    pause_gate: Option<Arc<AtomicBool>>,
    /// Runtime operations interface for graph mutations.
    runtime_ops: Arc<dyn RuntimeOperations>,
    /// Shared tokio runtime handle for async operations.
    tokio_handle: tokio::runtime::Handle,
}

impl RuntimeContext {
    pub fn new(
        gpu: GpuContext,
        runtime_id: Arc<RuntimeUniqueId>,
        runtime_ops: Arc<dyn RuntimeOperations>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            gpu,
            runtime_id,
            processor_id: None,
            pause_gate: None,
            runtime_ops,
            tokio_handle,
        }
    }

    /// Get the runtime's unique identifier.
    pub fn runtime_id(&self) -> &RuntimeUniqueId {
        &self.runtime_id
    }

    /// Get the processor's unique identifier (None for shared/global context).
    pub fn processor_id(&self) -> Option<&ProcessorUniqueId> {
        self.processor_id.as_ref()
    }

    /// Access runtime operations for graph mutations.
    ///
    /// Returns the runtime operations interface, allowing processors to add/remove
    /// processors and connections dynamically.
    pub fn runtime(&self) -> Arc<dyn RuntimeOperations> {
        Arc::clone(&self.runtime_ops)
    }

    /// Get the shared tokio runtime handle.
    ///
    /// Use this to spawn async tasks without creating your own runtime.
    /// The handle is shared across all processors.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// Create a processor-specific context with a processor ID.
    pub fn with_processor_id(&self, processor_id: ProcessorUniqueId) -> Self {
        Self {
            gpu: self.gpu.clone(),
            runtime_id: Arc::clone(&self.runtime_id),
            processor_id: Some(processor_id),
            pause_gate: self.pause_gate.clone(),
            runtime_ops: Arc::clone(&self.runtime_ops),
            tokio_handle: self.tokio_handle.clone(),
        }
    }

    /// Create a processor-specific context with a pause gate.
    pub fn with_pause_gate(&self, pause_gate: Arc<AtomicBool>) -> Self {
        Self {
            gpu: self.gpu.clone(),
            runtime_id: Arc::clone(&self.runtime_id),
            processor_id: self.processor_id.clone(),
            pause_gate: Some(pause_gate),
            runtime_ops: Arc::clone(&self.runtime_ops),
            tokio_handle: self.tokio_handle.clone(),
        }
    }

    /// Check if this processor is paused.
    ///
    /// For Manual mode processors, call this in your processing loop/callback
    /// to respect pause/resume requests. Returns `false` if no pause gate is set.
    pub fn is_paused(&self) -> bool {
        self.pause_gate
            .as_ref()
            .is_some_and(|gate| gate.load(Ordering::Acquire))
    }

    /// Check if processing should proceed (not paused).
    ///
    /// Convenience method - returns `true` if not paused.
    pub fn should_process(&self) -> bool {
        !self.is_paused()
    }

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// The "runtime thread" is the thread where StreamRuntime orchestration happens.
    /// On macOS, this is the main thread (NSApplication run loop) because Apple
    /// frameworks like AVFoundation, Metal, and CoreMedia require it.
    #[cfg(target_os = "macos")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        use objc2_foundation::NSThread;

        let is_runtime_thread = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on runtime thread, execute directly
        if is_runtime_thread {
            tracing::debug!(
                "[run_on_runtime_thread_async] already on runtime thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            f();
            return;
        }

        tracing::debug!(
            "[run_on_runtime_thread_async] on background thread ({:?} '{}'), dispatching to runtime thread",
            thread_id,
            thread_name
        );
        dispatch2::DispatchQueue::main().exec_async(f);
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// The "runtime thread" is the thread where StreamRuntime orchestration happens.
    /// On macOS, this is the main thread (NSApplication run loop) because Apple
    /// frameworks like AVFoundation, Metal, and CoreMedia require it.
    #[cfg(target_os = "macos")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        use objc2_foundation::NSThread;
        use std::sync::mpsc::channel;

        let is_runtime_thread = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on runtime thread, execute directly to avoid deadlock
        if is_runtime_thread {
            tracing::debug!(
                "[run_on_runtime_thread_blocking] already on runtime thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            return f();
        }

        tracing::debug!("[run_on_runtime_thread_blocking] on background thread ({:?} '{}'), dispatching to runtime thread and waiting", thread_id, thread_name);
        let (tx, rx) = channel();

        dispatch2::DispatchQueue::main().exec_async(move || {
            let result = f();
            let _ = tx.send(result);
        });

        rx.recv()
            .expect("Failed to receive result from runtime thread")
    }

    // =========================================================================
    // Windows Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `PostMessage` to dispatch
    /// to the runtime thread's Win32 message loop. Required for:
    /// - DirectX/Direct3D device creation and rendering
    /// - Win32 window creation (HWND)
    /// - COM objects with STA threading requirements
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Windows runtime thread dispatch via PostMessage/SendMessage
        // to the runtime thread's message loop (GetMessage/DispatchMessage pump).
        //
        // Implementation approach:
        // 1. Store runtime thread ID at startup (GetCurrentThreadId)
        // 2. Create a hidden HWND for receiving messages
        // 3. PostMessage with custom WM_USER message containing boxed closure
        // 4. Message loop handler unboxes and executes closure
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `SendMessage` (blocking) or
    /// `PostMessage` + event synchronization. Required for same reasons as async variant.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Windows runtime thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same as async, but use SendMessage (blocks until processed) OR
        // 2. PostMessage + ManualResetEvent for completion signaling
        // 3. Return result via shared memory or channel
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Linux Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Implementation depends on windowing system:
    /// - **X11**: Use `XSendEvent` or integrate with GTK/Qt main loop
    /// - **Wayland**: Use `wl_display_dispatch` or GTK/Qt integration
    /// - **Headless**: May not require runtime thread dispatch
    ///
    /// For GTK: `glib::MainContext::default().invoke()`
    /// For Qt: `QMetaObject::invokeMethod` with `Qt::QueuedConnection`
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Linux runtime thread dispatch.
        //
        // Implementation approach (GTK/glib):
        // 1. Use glib::MainContext::default().invoke(f)
        // 2. Requires glib dependency and running GMainLoop on runtime thread
        //
        // Alternative (custom):
        // 1. Create eventfd or pipe at startup
        // 2. Runtime thread polls the fd in its event loop
        // 3. Write closure pointer to fd, runtime thread reads and executes
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Same considerations as async variant,
    /// plus synchronization for returning result.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Linux runtime thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same dispatch mechanism as async
        // 2. Include oneshot channel or CondVar for result
        // 3. Block on channel/condvar until runtime thread signals completion
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Fallback Implementation (other platforms)
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No runtime thread dispatch implemented.
    /// Executes directly on the calling thread.
    ///
    /// If you need runtime thread dispatch on an unsupported platform,
    /// please file an issue with your platform requirements.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No runtime thread dispatch implemented.
    /// Executes directly on the calling thread.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Platform Setup
    // =========================================================================

    /// Ensure the platform is ready for runtime operations.
    ///
    /// This method handles platform-specific initialization that must complete
    /// before processors can safely use platform APIs. On macOS, this sets up
    /// NSApplication and verifies the app has finished launching.
    ///
    /// Call this from `StreamRuntime::start()` after GPU context is initialized
    /// but before starting any processors.
    #[cfg(target_os = "macos")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        // Detect if we're running as a standalone app
        // (not embedded in another app with its own NSApplication event loop)
        let is_standalone = if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            !app.isRunning()
        } else {
            // Not on runtime thread - can't check NSApplication state
            false
        };

        if is_standalone {
            tracing::info!("[ensure_platform_ready] Setting up macOS application");
            crate::apple::runtime_ext::setup_macos_app();

            // CRITICAL: Verify the macOS platform is fully ready BEFORE starting
            // any processors. This uses Apple's NSRunningApplication.isFinishedLaunching
            // API to confirm the app has completed its launch sequence.
            tracing::info!("[ensure_platform_ready] Verifying macOS platform readiness...");
            crate::apple::runtime_ext::ensure_macos_platform_ready()?;
        }

        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On Windows, this is a no-op placeholder for future Win32 setup.
    #[cfg(target_os = "windows")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        // TODO: Future Windows-specific initialization
        // - DirectX device validation
        // - Win32 message loop setup
        tracing::debug!("[ensure_platform_ready] Windows - no setup required");
        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On Linux, this is a no-op placeholder for future X11/Wayland setup.
    #[cfg(target_os = "linux")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        // TODO: Future Linux-specific initialization
        // - X11/Wayland connection setup
        // - GPU driver validation
        tracing::debug!("[ensure_platform_ready] Linux - no setup required");
        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On unsupported platforms, this is a no-op.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        tracing::debug!("[ensure_platform_ready] Unsupported platform - no setup required");
        Ok(())
    }
}

// Unit tests removed - these require NSApplication run loop which isn't available in test harness.
// See examples/test-main-thread-dispatch for validation of this functionality.
