//! Simple passthrough processor - demonstrates StreamTransform trait
//!
//! This processor serves as a reference implementation showing how to implement
//! the StreamElement + StreamTransform trait hierarchy for simple effect processors.

use crate::core::{Result, StreamInput, StreamOutput, VideoFrame};
use crate::core::traits::{StreamElement, StreamTransform, ElementType};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME};
use crate::core::RuntimeContext;
use serde::{Serialize, Deserialize};
use std::sync::Arc;

/// Configuration for SimplePassthroughProcessor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplePassthroughConfig {
    /// Scale factor (currently unused, for demonstration)
    pub scale: f32,
}

impl Default for SimplePassthroughConfig {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

/// Simple passthrough processor using StreamTransform trait
///
/// This is a reference implementation demonstrating the recommended pattern
/// for creating new transform processors. It shows:
/// - How to implement StreamElement base trait
/// - How to implement StreamTransform specialized trait
/// - Simple 1→1 input/output configuration
/// - Reactive processing (reads when data available)
///
/// # Example
///
/// ```ignore
/// use streamlib::SimplePassthroughProcessor;
///
/// // Create via config-based API
/// let processor = SimplePassthroughProcessor::from_config(
///     SimplePassthroughConfig { scale: 1.0 }
/// )?;
/// ```
pub struct SimplePassthroughProcessor {
    /// Processor name
    name: String,

    /// Input video stream
    input: StreamInput<VideoFrame>,

    /// Output video stream
    output: StreamOutput<VideoFrame>,

    /// Config field - scale factor
    scale: f32,
}

// Implement base StreamElement trait
impl StreamElement for SimplePassthroughProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <Self as StreamTransform>::descriptor()
    }

    fn start(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        // Stateless processor - nothing to initialize
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stateless processor - nothing to clean up
        Ok(())
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "input".to_string(),
            schema: Arc::clone(&SCHEMA_VIDEO_FRAME),
            required: true,
            description: "Input video stream".to_string(),
        }]
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "output".to_string(),
            schema: Arc::clone(&SCHEMA_VIDEO_FRAME),
            required: true,
            description: "Output video stream".to_string(),
        }]
    }

    fn as_transform(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// Implement specialized StreamTransform trait
impl StreamTransform for SimplePassthroughProcessor {
    type Config = SimplePassthroughConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            name: "simple_passthrough".to_string(),
            input: StreamInput::new("input"),
            output: StreamOutput::new("output"),
            scale: config.scale,
        })
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            // In a real processor, you might scale/transform the frame here
            // For now, just pass it through
            self.output.write(frame);
        }
        Ok(())
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "SimplePassthroughProcessor",
                "Passes video frames through unchanged (for testing)"
            )
            .with_usage_context("Connect video input and output for pipeline testing")
            .with_input(PortDescriptor {
                name: "input".to_string(),
                schema: Arc::clone(&SCHEMA_VIDEO_FRAME),
                required: true,
                description: "Input video stream".to_string(),
            })
            .with_output(PortDescriptor {
                name: "output".to_string(),
                schema: Arc::clone(&SCHEMA_VIDEO_FRAME),
                required: true,
                description: "Output video stream".to_string(),
            })
            .with_tags(vec!["transform", "video", "test", "passthrough"])
        )
    }
}

impl SimplePassthroughProcessor {
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

    #[test]
    fn test_processor_can_be_created_from_config() {
        let config = SimplePassthroughConfig {
            scale: 2.0,
        };

        let processor = SimplePassthroughProcessor::from_config(config);
        assert!(processor.is_ok());

        let processor = processor.unwrap();
        assert_eq!(processor.scale(), 2.0);
        assert_eq!(processor.name(), "simple_passthrough");
    }

    #[test]
    fn test_processor_has_descriptor() {
        let desc = SimplePassthroughProcessor::descriptor();
        assert!(desc.is_some());

        let desc = desc.unwrap();
        assert_eq!(desc.name, "SimplePassthroughProcessor");
        assert!(desc.description.contains("Passes video frames"));
    }

    #[test]
    fn test_element_type() {
        let config = SimplePassthroughConfig::default();
        let processor = SimplePassthroughProcessor::from_config(config).unwrap();

        assert_eq!(processor.element_type(), ElementType::Transform);
    }

    #[test]
    fn test_input_output_ports() {
        let config = SimplePassthroughConfig::default();
        let processor = SimplePassthroughProcessor::from_config(config).unwrap();

        let inputs = processor.input_ports();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "input");
        assert_eq!(inputs[0].schema.name, "VideoFrame");

        let outputs = processor.output_ports();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "output");
        assert_eq!(outputs[0].schema.name, "VideoFrame");
    }

    #[test]
    fn test_downcast_to_transform() {
        let config = SimplePassthroughConfig::default();
        let processor = SimplePassthroughProcessor::from_config(config).unwrap();

        // Should successfully downcast to transform
        assert!(processor.as_transform().is_some());

        // Should NOT downcast to source or sink
        assert!(processor.as_source().is_none());
        assert!(processor.as_sink().is_none());
    }

    #[test]
    fn test_scale_getter_setter() {
        let config = SimplePassthroughConfig { scale: 1.0 };
        let mut processor = SimplePassthroughProcessor::from_config(config).unwrap();

        assert_eq!(processor.scale(), 1.0);

        processor.set_scale(2.5);
        assert_eq!(processor.scale(), 2.5);
    }

    #[test]
    fn test_process_passes_through() {
        let config = SimplePassthroughConfig::default();
        let mut processor = SimplePassthroughProcessor::from_config(config).unwrap();

        // Process should succeed even with no data
        assert!(processor.process().is_ok());
    }
}
