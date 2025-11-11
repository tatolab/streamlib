
use crate::core::{Result, StreamInput, StreamOutput, VideoFrame};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME};
use crate::core::RuntimeContext;
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use streamlib_macros::StreamProcessor;

// Re-export for macro use (macro expects `streamlib::` path)
use crate as streamlib;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplePassthroughConfig {
    pub scale: f32,
}

impl Default for SimplePassthroughConfig {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

// NEW PATTERN: Complete trait generation with generate_impls = true!
#[derive(StreamProcessor)]
#[processor(
    generate_impls = true,
    config = SimplePassthroughConfig,
    name = "SimplePassthroughProcessor",
    description = "Passes video frames through unchanged (for testing)"
)]
pub struct SimplePassthroughProcessor {
    // Config fields (non-ports)
    scale: f32,

    // Port fields - annotated with descriptions!
    #[input(description = "Input video stream")]
    input: StreamInput<VideoFrame>,

    #[output(description = "Output video stream")]
    output: StreamOutput<VideoFrame>,
}

// Only business logic implementation needed!
impl SimplePassthroughProcessor {
    fn process(&mut self) -> Result<()> {
        // Direct field access - no nested ports struct!
        if let Some(frame) = self.input.read_latest() {
            self.output.write(frame);
        }
        Ok(())
    }
}

impl SimplePassthroughProcessor {
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::PortType;

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

        assert!(processor.as_transform().is_some());

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

        assert!(processor.process().is_ok());
    }

    #[test]
    fn test_port_introspection() {
        let config = SimplePassthroughConfig::default();
        let processor = SimplePassthroughProcessor::from_config(config).unwrap();

        // Test macro-generated port type lookups
        assert_eq!(processor.get_input_port_type("input"), Some(PortType::Video));
        assert_eq!(processor.get_input_port_type("nonexistent"), None);

        assert_eq!(processor.get_output_port_type("output"), Some(PortType::Video));
        assert_eq!(processor.get_output_port_type("nonexistent"), None);
    }

    #[test]
    fn test_ports_convenience_method() {
        let config = SimplePassthroughConfig::default();
        let processor = SimplePassthroughProcessor::from_config(config).unwrap();

        // Test macro-generated ports() method
        let ports = processor.ports();

        // Access via ports() method (backward compatibility)
        let _input_ref = ports.inputs.input;
        let _output_ref = ports.outputs.output;
    }
}
