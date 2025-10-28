//! Platform-configured StreamRuntime
//!
//! This module provides a StreamRuntime that automatically configures itself
//! for the current platform. Users just call `StreamRuntime::new(60.0)` and
//! the runtime handles platform-specific setup (like NSApplication on macOS).

use crate::core::{Result, StreamProcessor, StreamInput, StreamOutput, ports::PortMessage};

/// Platform-configured StreamRuntime
///
/// This wraps `crate::core::StreamRuntime` and automatically configures
/// platform-specific features on construction.
pub struct StreamRuntime {
    inner: crate::core::StreamRuntime,
}

impl StreamRuntime {
    /// Create a new runtime configured for the current platform
    pub fn new(fps: f64) -> Self {
        let mut inner = crate::core::StreamRuntime::new(fps);

        // Configure platform-specific event loop
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            crate::apple::runtime_ext::configure_macos_event_loop(&mut inner);
        }

        Self { inner }
    }

    /// Add a processor to the runtime
    pub fn add_processor(&mut self, processor: Box<dyn StreamProcessor>) {
        self.inner.add_processor(processor);
    }

    /// Connect two ports
    pub fn connect<T: PortMessage>(&mut self, output: &mut StreamOutput<T>, input: &mut StreamInput<T>) -> Result<()> {
        self.inner.connect(output, input)
    }

    /// Start the runtime
    pub async fn start(&mut self) -> Result<()> {
        self.inner.start().await
    }

    /// Run the runtime until stopped
    pub async fn run(&mut self) -> Result<()> {
        self.inner.run().await
    }

    /// Stop the runtime
    pub async fn stop(&mut self) -> Result<()> {
        self.inner.stop().await
    }
}
