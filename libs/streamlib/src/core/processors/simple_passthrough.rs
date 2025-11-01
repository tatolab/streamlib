//! Simple passthrough processor - demonstrates StreamProcessor macro usage
//!
//! This processor serves as a reference implementation showing how to use
//! the `#[derive(StreamProcessor)]` macro for simple effect processors.

use crate::core::{Result, StreamInput, StreamOutput, VideoFrame};
use streamlib_macros::StreamProcessor;

/// Simple passthrough processor using the StreamProcessor macro
///
/// This is a reference implementation demonstrating the recommended pattern
/// for creating new processors. The macro automatically generates:
/// - Config struct with the `scale` field
/// - `from_config()` constructor
/// - `descriptor()` with type-safe schemas
/// - Smart defaults for descriptions, tags, and examples
///
/// # Example
///
/// ```ignore
/// use streamlib::SimplePassthroughProcessor;
///
/// // Create via config-based API
/// let processor = runtime.add_processor_with_config::<SimplePassthroughProcessor>(
///     SimplePassthroughProcessorConfig { scale: 1.0 }
/// )?;
/// ```
#[derive(StreamProcessor)]
#[processor(
    description = "Passes video frames through unchanged (for testing)",
    usage = "Connect video input and output for pipeline testing"
)]
pub struct SimplePassthroughProcessor {
    #[input(description = "Input video stream")]
    input: StreamInput<VideoFrame>,

    #[output(description = "Output video stream")]
    output: StreamOutput<VideoFrame>,

    // Config field - automatically becomes part of Config struct
    scale: f32,
}

impl SimplePassthroughProcessor {
    /// Process implementation - just passes frames through
    pub fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            // In a real processor, you might scale/transform the frame here
            // For now, just pass it through
            self.output.write(frame);
        }
        Ok(())
    }

    /// Get current scale value
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Set scale value
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::StreamProcessor;

    #[test]
    fn test_processor_can_be_created_from_config() {
        // The macro should have generated this
        let config = Config {
            scale: 2.0,
        };

        // This should compile if the macro worked
        let processor = SimplePassthroughProcessor::from_config(config);
        assert!(processor.is_ok());
    }

    #[test]
    fn test_processor_has_descriptor() {
        // The macro should have generated a descriptor
        let desc = SimplePassthroughProcessor::descriptor();
        assert!(desc.is_some());

        let desc = desc.unwrap();
        assert_eq!(desc.name, "SimplePassthroughProcessor");
        assert!(desc.description.contains("Passes video frames"));
    }
}
