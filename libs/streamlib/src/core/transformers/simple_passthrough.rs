
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

// NEW PATTERN: Ports directly on processor struct!
#[derive(StreamProcessor)]
pub struct SimplePassthroughProcessor {
    // Config fields (non-ports)
    name: String,
    scale: f32,

    // Port fields - annotated!
    #[input]
    input: StreamInput<VideoFrame>,

    #[output]
    output: StreamOutput<VideoFrame>,
}

impl StreamElement for SimplePassthroughProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <Self as StreamProcessor>::descriptor()
    }

    fn start(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
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

// Manual StreamProcessor implementation
impl StreamProcessor for SimplePassthroughProcessor {
    type Config = SimplePassthroughConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            name: "simple_passthrough".to_string(),
            scale: config.scale,
            // Port construction
            input: StreamInput::new("input"),
            output: StreamOutput::new("output"),
        })
    }

    fn process(&mut self) -> Result<()> {
        // Direct field access - no nested ports struct!
        if let Some(frame) = self.input.read_latest() {
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

    // Delegate to macro-generated methods
    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_input_port_type_impl(port_name)
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_output_port_type_impl(port_name)
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        self.wire_input_connection_impl(port_name, connection)
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        self.wire_output_connection_impl(port_name, connection)
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
