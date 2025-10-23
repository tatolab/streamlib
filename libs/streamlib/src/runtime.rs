//! Platform-configured StreamRuntime
//!
//! This module provides a StreamRuntime that automatically configures itself
//! for the current platform. Users just call `StreamRuntime::new(60.0)` and
//! the runtime handles platform-specific setup (like NSApplication on macOS).

use streamlib_core::{Result, StreamProcessor, StreamInput, StreamOutput, ports::PortMessage};

/// Platform-configured StreamRuntime
///
/// This wraps `streamlib_core::StreamRuntime` and automatically configures
/// platform-specific features on construction.
///
/// # Example
///
/// ```ignore
/// use streamlib::StreamRuntime;
///
/// // Automatically configured for the platform
/// let mut runtime = StreamRuntime::new(60.0);
///
/// runtime.add_processor(Box::new(camera));
/// runtime.add_processor(Box::new(display));
/// runtime.start().await?;
/// runtime.run().await?;
/// ```
pub struct StreamRuntime {
    inner: streamlib_core::StreamRuntime,
}

impl StreamRuntime {
    /// Create a new runtime configured for the current platform
    ///
    /// On macOS/iOS: Configures NSApplication event loop
    /// On Linux: Uses default tokio runtime
    /// On Windows: Uses default tokio runtime
    ///
    /// # Arguments
    ///
    /// * `fps` - Target frames per second for the clock
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib::StreamRuntime;
    ///
    /// let runtime = StreamRuntime::new(60.0);
    /// ```
    pub fn new(fps: f64) -> Self {
        let mut inner = streamlib_core::StreamRuntime::new(fps);

        // Configure platform-specific event loop
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            streamlib_apple::runtime_ext::configure_macos_event_loop(&mut inner);
        }

        Self { inner }
    }

    /// Add a processor to the runtime
    ///
    /// # Arguments
    ///
    /// * `processor` - Boxed processor implementation
    pub fn add_processor(&mut self, processor: Box<dyn StreamProcessor>) {
        self.inner.add_processor(processor);
    }

    /// Connect two ports
    ///
    /// # Arguments
    ///
    /// * `output` - Output port to connect from
    /// * `input` - Input port to connect to
    pub fn connect<T: PortMessage>(&mut self, output: &mut StreamOutput<T>, input: &mut StreamInput<T>) -> Result<()> {
        self.inner.connect(output, input)
    }

    /// Start the runtime
    ///
    /// Spawns clock task and processor threads.
    pub async fn start(&mut self) -> Result<()> {
        self.inner.start().await
    }

    /// Run the runtime until stopped
    ///
    /// Blocks until Ctrl+C or stop() is called.
    /// Automatically handles platform-specific event loops.
    pub async fn run(&mut self) -> Result<()> {
        self.inner.run().await
    }

    /// Stop the runtime
    ///
    /// Cleanly shuts down all processors and threads.
    pub async fn stop(&mut self) -> Result<()> {
        self.inner.stop().await
    }
}
