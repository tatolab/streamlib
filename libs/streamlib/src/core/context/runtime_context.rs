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
    ///
    /// This is useful for platform APIs that require execution on the main thread
    /// (e.g., AVFoundation on macOS). The closure will be queued on the main dispatch
    /// queue and executed when the main thread's event loop processes it.
    ///
    /// # Platform Notes
    ///
    /// - **macOS**: Uses GCD's `DispatchQueue::main()` which integrates with NSApplication's event loop
    /// - The main thread must be running an event loop (via `runtime.run()`)
    /// - Closures queued before the event loop starts will execute once it begins
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use streamlib::core::RuntimeContext;
    /// # fn example(ctx: &RuntimeContext) {
    /// ctx.run_on_main_async(|| {
    ///     // This code executes on main thread
    ///     println!("Running on main thread");
    /// });
    /// # }
    /// ```
    #[cfg(target_os = "macos")]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        dispatch2::DispatchQueue::main().exec_async(f);
    }

    /// Dispatch a closure to execute on the main thread and wait for the result.
    ///
    /// This blocks the calling thread until the closure completes on the main thread.
    /// Use this when you need a return value or must ensure completion before proceeding.
    ///
    /// # Platform Notes
    ///
    /// - **macOS**: Uses GCD's `DispatchQueue::main()` with channel-based synchronization
    /// - The main thread must be running an event loop
    /// - Calling this FROM the main thread will deadlock
    ///
    /// # Panics
    ///
    /// Panics if the main thread fails to execute the closure or send back the result.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use streamlib::core::RuntimeContext;
    /// # fn example(ctx: &RuntimeContext) {
    /// let result = ctx.run_on_main_blocking(|| {
    ///     // This code executes on main thread
    ///     42
    /// });
    /// assert_eq!(result, 42);
    /// # }
    /// ```
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

    /// No-op implementation for non-macOS platforms
    #[cfg(not(target_os = "macos"))]
    pub fn run_on_main_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // Just execute immediately on current thread
        f();
    }

    /// No-op implementation for non-macOS platforms
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
