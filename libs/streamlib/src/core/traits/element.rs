
use crate::core::schema::{ProcessorDescriptor, PortDescriptor};
use crate::core::error::Result;
use crate::core::RuntimeContext;
use serde::{Deserialize, Serialize};

pub trait ProcessorConfig: Serialize + for<'de> Deserialize<'de> {
    type Processor: StreamElement;
    fn build(self) -> Result<Self::Processor>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Source,

    Sink,

    Transform,
}

pub trait StreamElement: Send + 'static {
    fn name(&self) -> &str;

    fn element_type(&self) -> ElementType;

    fn descriptor(&self) -> Option<ProcessorDescriptor>;


    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())  // Default: no-op
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())  // Default: no-op
    }

    fn shutdown(&mut self) -> Result<()> {
        self.__generated_teardown()
    }


    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }


    fn as_source(&self) -> Option<&dyn std::any::Any> {
        None
    }

    fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    fn as_sink(&self) -> Option<&dyn std::any::Any> {
        None
    }

    fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    fn as_transform(&self) -> Option<&dyn std::any::Any> {
        None
    }

    fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::source::StreamSource;

    struct MockSource {
        name: String,
    }

    impl StreamElement for MockSource {
        fn name(&self) -> &str {
            &self.name
        }

        fn element_type(&self) -> ElementType {
            ElementType::Source
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }

        fn output_ports(&self) -> Vec<PortDescriptor> {
            use crate::core::schema::SCHEMA_VIDEO_FRAME;
            vec![PortDescriptor {
                name: "video".to_string(),
                schema: SCHEMA_VIDEO_FRAME.clone(),
                required: true,
                description: "Test video output".to_string(),
            }]
        }

        fn as_source(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }


    #[test]
    fn test_element_type() {
        let source = MockSource {
            name: "test_source".to_string(),
        };
        assert_eq!(source.element_type(), ElementType::Source);
    }

    #[test]
    fn test_element_name() {
        let source = MockSource {
            name: "camera_0".to_string(),
        };
        assert_eq!(source.name(), "camera_0");
    }

    #[test]
    fn test_output_ports() {
        let source = MockSource {
            name: "test".to_string(),
        };
        let ports = source.output_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].name, "video");
        assert_eq!(ports[0].schema.name, "VideoFrame");
        assert_eq!(ports[0].description, "Test video output");
        assert!(ports[0].required);
    }

    #[test]
    fn test_downcast_to_source() {
        let source = MockSource {
            name: "test".to_string(),
        };
        assert!(source.as_source().is_some());
        assert!(source.as_sink().is_none());
        assert!(source.as_transform().is_none());
    }

    #[test]
    fn test_lifecycle_defaults() {
        let mut source = MockSource {
            name: "test".to_string(),
        };

        assert!(source.teardown().is_ok());
        assert!(source.shutdown().is_ok());
    }

    #[test]
    fn test_element_type_equality() {
        assert_eq!(ElementType::Source, ElementType::Source);
        assert_ne!(ElementType::Source, ElementType::Sink);
        assert_ne!(ElementType::Sink, ElementType::Transform);
    }

    #[test]
    fn test_element_type_debug() {
        let source_type = ElementType::Source;
        let debug_str = format!("{:?}", source_type);
        assert_eq!(debug_str, "Source");
    }
}
