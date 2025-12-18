// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::GpuContext;

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
    /// Pause gate for this processor (None for shared/global context).
    pause_gate: Option<Arc<AtomicBool>>,
}

impl RuntimeContext {
    pub fn new(gpu: GpuContext) -> Self {
        Self {
            gpu,
            pause_gate: None,
        }
    }

    /// Create a processor-specific context with a pause gate.
    pub fn with_pause_gate(&self, pause_gate: Arc<AtomicBool>) -> Self {
        Self {
            gpu: self.gpu.clone(),
            pause_gate: Some(pause_gate),
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

    /// Dispatch a closure to execute on the main thread asynchronously.
    #[cfg(target_os = "macos")]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        use objc2_foundation::NSThread;

        let is_main = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on main thread, execute directly
        if is_main {
            tracing::debug!(
                "[run_on_main_async] already on main thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            f();
            return;
        }

        tracing::debug!(
            "[run_on_main_async] on background thread ({:?} '{}'), dispatching to main",
            thread_id,
            thread_name
        );
        dispatch2::DispatchQueue::main().exec_async(f);
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    #[cfg(target_os = "macos")]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        use objc2_foundation::NSThread;
        use std::sync::mpsc::channel;

        let is_main = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on main thread, execute directly to avoid deadlock
        if is_main {
            tracing::debug!(
                "[run_on_main_blocking] already on main thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            return f();
        }

        tracing::debug!("[run_on_main_blocking] on background thread ({:?} '{}'), dispatching to main and waiting", thread_id, thread_name);
        let (tx, rx) = channel();

        dispatch2::DispatchQueue::main().exec_async(move || {
            let result = f();
            let _ = tx.send(result);
        });

        rx.recv()
            .expect("Failed to receive result from main thread")
    }

    // =========================================================================
    // Windows Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the main thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `PostMessage` to dispatch
    /// to the main thread's Win32 message loop. Required for:
    /// - DirectX/Direct3D device creation and rendering
    /// - Win32 window creation (HWND)
    /// - COM objects with STA threading requirements
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Windows main thread dispatch via PostMessage/SendMessage
        // to the main thread's message loop (GetMessage/DispatchMessage pump).
        //
        // Implementation approach:
        // 1. Store main thread ID at startup (GetCurrentThreadId)
        // 2. Create a hidden HWND for receiving messages
        // 3. PostMessage with custom WM_USER message containing boxed closure
        // 4. Message loop handler unboxes and executes closure
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_async] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `SendMessage` (blocking) or
    /// `PostMessage` + event synchronization. Required for same reasons as async variant.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Windows main thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same as async, but use SendMessage (blocks until processed) OR
        // 2. PostMessage + ManualResetEvent for completion signaling
        // 3. Return result via shared memory or channel
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_blocking] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Linux Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the main thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Implementation depends on windowing system:
    /// - **X11**: Use `XSendEvent` or integrate with GTK/Qt main loop
    /// - **Wayland**: Use `wl_display_dispatch` or GTK/Qt integration
    /// - **Headless**: May not require main thread dispatch
    ///
    /// For GTK: `glib::MainContext::default().invoke()`
    /// For Qt: `QMetaObject::invokeMethod` with `Qt::QueuedConnection`
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Linux main thread dispatch.
        //
        // Implementation approach (GTK/glib):
        // 1. Use glib::MainContext::default().invoke(f)
        // 2. Requires glib dependency and running GMainLoop on main thread
        //
        // Alternative (custom):
        // 1. Create eventfd or pipe at startup
        // 2. Main thread polls the fd in its event loop
        // 3. Write closure pointer to fd, main thread reads and executes
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_async] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Same considerations as async variant,
    /// plus synchronization for returning result.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Linux main thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same dispatch mechanism as async
        // 2. Include oneshot channel or CondVar for result
        // 3. Block on channel/condvar until main thread signals completion
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_blocking] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Fallback Implementation (other platforms)
    // =========================================================================

    /// Dispatch a closure to execute on the main thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No main thread dispatch implemented.
    /// Executes directly on the calling thread.
    ///
    /// If you need main thread dispatch on an unsupported platform,
    /// please file an issue with your platform requirements.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_async] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No main thread dispatch implemented.
    /// Executes directly on the calling thread.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_blocking] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }
}

// Unit tests removed - these require NSApplication run loop which isn't available in test harness.
// See examples/test-main-thread-dispatch for validation of this functionality.
