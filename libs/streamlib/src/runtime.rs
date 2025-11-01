//! Platform-configured StreamRuntime
//!
//! This module provides a StreamRuntime that automatically configures itself
//! for the current platform. Users just call `StreamRuntime::new()` and
//! the runtime handles platform-specific setup (like NSApplication on macOS).

use crate::core::{Result, StreamProcessor, ports::PortMessage};

// Re-export AudioConfig from core
pub use crate::core::runtime::AudioConfig;

/// Platform-configured StreamRuntime
///
/// This wraps `crate::core::StreamRuntime` and automatically configures
/// platform-specific features on construction.
pub struct StreamRuntime {
    inner: crate::core::StreamRuntime,
}

impl StreamRuntime {
    /// Create a new runtime configured for the current platform
    pub fn new() -> Self {
        let mut inner = crate::core::StreamRuntime::new();

        // Platform-specific configuration
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            crate::apple::configure_macos_event_loop(&mut inner);
        }

        Self { inner }
    }

    /// Add a processor to the runtime (legacy API - prefer add_processor::<P>())
    pub fn add_processor_runtime(
        &mut self,
        processor: Box<dyn crate::core::stream_processor::DynStreamProcessor>,
    ) -> Result<String> {
        // This is the old API - new code should use add_processor::<P>()
        tokio::runtime::Handle::current().block_on(async {
            self.inner.add_processor_runtime(processor).await
        })
    }

    /// Connect two ports using handles (new API)
    pub fn connect<T: PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        self.inner.connect(output, input)
    }

    /// Add a processor with default config
    pub fn add_processor<P: StreamProcessor>(&mut self) -> Result<crate::core::handles::ProcessorHandle> {
        self.inner.add_processor::<P>()
    }

    /// Add a processor with custom config
    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<crate::core::handles::ProcessorHandle> {
        self.inner.add_processor_with_config::<P>(config)
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

    /// Get the global audio configuration
    ///
    /// All audio processors should use these settings to ensure
    /// sample rate compatibility across the pipeline.
    pub fn audio_config(&self) -> AudioConfig {
        self.inner.audio_config()
    }

    /// Set the global audio configuration
    ///
    /// **Must be called before starting the runtime**. Changing audio config
    /// after processors are running may cause sample rate mismatches.
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        self.inner.set_audio_config(config)
    }

    /// Validate that an AudioFrame matches the runtime's audio configuration
    ///
    /// This checks that the frame's sample rate and channel count match the
    /// runtime's global audio config.
    pub fn validate_audio_frame(&self, frame: &crate::core::AudioFrame) -> Result<()> {
        self.inner.validate_audio_frame(frame)
    }
}
