use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::RuntimeContext;

/// Classification of processor types for graph analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    /// Produces data (no inputs, has outputs)
    Source,
    /// Consumes data (has inputs, no outputs)
    Sink,
    /// Transforms data (has both inputs and outputs)
    Transform,
}

/// Base trait for all stream elements.
///
/// Provides identity and metadata. Implemented by all processors.
pub trait StreamElement: Send + 'static {
    fn name(&self) -> &str;

    fn element_type(&self) -> ElementType;

    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_lifecycle_defaults() {
        let mut source = MockSource {
            name: "test".to_string(),
        };
        assert!(source.__generated_teardown().is_ok());
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
