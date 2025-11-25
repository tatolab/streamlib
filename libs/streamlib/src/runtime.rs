use crate::core::{bus::PortMessage, Result};

pub use crate::core::handles::ProcessorHandle;
pub use crate::core::runtime::state::RuntimeState;
pub use crate::core::runtime::{compute_delta, ExecutionDelta};

pub struct StreamRuntime {
    inner: crate::core::StreamRuntime,
}

impl Default for StreamRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRuntime {
    pub fn new() -> Self {
        let mut inner = crate::core::StreamRuntime::new();

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            crate::apple::configure_macos_event_loop(&mut inner);
        }

        Self { inner }
    }

    pub fn add_processor_with_config<P>(&mut self, config: P::Config) -> Result<ProcessorHandle>
    where
        P: crate::core::traits::StreamProcessor + 'static,
    {
        self.inner.add_processor_with_config::<P>(config)
    }

    pub fn add_processor<P>(&mut self) -> Result<ProcessorHandle>
    where
        P: crate::core::traits::StreamProcessor + 'static,
    {
        self.inner.add_processor::<P>()
    }

    pub fn connect<T: PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<String> {
        self.inner.connect(output, input)
    }

    pub fn disconnect<T: PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        self.inner.disconnect(output, input)
    }

    pub fn disconnect_by_id(&mut self, connection_id: &str) -> Result<()> {
        self.inner.disconnect_by_id(&connection_id.to_string())
    }

    pub fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    pub fn run(&mut self) -> Result<()> {
        self.inner.run()
    }

    pub fn stop(&mut self) -> Result<()> {
        self.inner.stop()
    }

    /// Pause the runtime (suspend processor threads, keep state)
    ///
    /// While paused:
    /// - Processor threads are suspended (not executing process())
    /// - Graph can be modified (add/remove processors, connect/disconnect)
    /// - State is preserved (no teardown)
    /// - Use `resume()` to continue execution
    pub fn pause(&mut self) -> Result<()> {
        self.inner.pause()
    }

    /// Resume the runtime from paused state
    ///
    /// If the graph was modified while paused, this will trigger recompilation
    /// before resuming execution.
    pub fn resume(&mut self) -> Result<()> {
        self.inner.resume()
    }

    /// Restart the runtime (stop and start with the same graph)
    ///
    /// This is useful for applying graph changes that require full re-initialization,
    /// or for recovering from errors.
    pub fn restart(&mut self) -> Result<()> {
        self.inner.restart()
    }

    /// Get the current runtime state
    pub fn state(&self) -> RuntimeState {
        self.inner.state()
    }

    /// Access the graph for inspection
    pub fn graph(&self) -> &crate::core::graph::Graph {
        self.inner.graph()
    }

    /// Request camera permission from the system.
    /// Must be called on the main thread before adding camera processors.
    /// Returns true if permission is granted, false if denied.
    pub fn request_camera(&self) -> Result<bool> {
        self.inner.request_camera()
    }

    /// Request microphone permission from the system.
    /// Must be called on the main thread before adding audio capture processors.
    /// Returns true if permission is granted, false if denied.
    pub fn request_microphone(&self) -> Result<bool> {
        self.inner.request_microphone()
    }
}
