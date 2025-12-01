use super::GpuContext;

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
}

impl RuntimeContext {
    pub fn new(gpu: GpuContext) -> Self {
        Self { gpu }
    }

    /// Dispatch a closure to execute on the main thread asynchronously.
    #[cfg(target_os = "macos")]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        dispatch2::DispatchQueue::main().exec_async(f);
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    #[cfg(target_os = "macos")]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        use std::sync::mpsc::channel;
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
        // Just execute immediately on current thread
        f();
    }

    #[cfg(not(target_os = "macos"))]
    pub fn run_on_main_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // Just execute immediately on current thread
        f()
    }
}

// Unit tests removed - these require NSApplication run loop which isn't available in test harness.
// See examples/test-main-thread-dispatch for validation of this functionality.
