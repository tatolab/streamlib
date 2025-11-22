use crate::core::{bus::PortMessage, Result};

pub use crate::core::handles::ProcessorHandle;

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
