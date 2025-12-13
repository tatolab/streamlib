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

    #[cfg(not(target_os = "macos"))]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_async] non-macOS, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    #[cfg(not(target_os = "macos"))]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current().name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_main_blocking] non-macOS, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }
}

// Unit tests removed - these require NSApplication run loop which isn't available in test harness.
// See examples/test-main-thread-dispatch for validation of this functionality.
