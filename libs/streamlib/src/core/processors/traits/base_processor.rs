use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::RuntimeContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorType {
    /// Produces data (no inputs, has outputs).
    Source,
    /// Consumes data (has inputs, no outputs).
    Sink,
    /// Transforms data (has both inputs and outputs).
    Transform,
}

pub trait BaseProcessor: Send + 'static {
    fn name(&self) -> &str;

    fn processor_type(&self) -> ProcessorType;

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

    impl BaseProcessor for MockSource {
        fn name(&self) -> &str {
            &self.name
        }

        fn processor_type(&self) -> ProcessorType {
            ProcessorType::Source
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }
    }

    #[test]
    fn test_processor_type() {
        let source = MockSource {
            name: "test_source".to_string(),
        };
        assert_eq!(source.processor_type(), ProcessorType::Source);
    }

    #[test]
    fn test_processor_name() {
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
    fn test_processor_type_equality() {
        assert_eq!(ProcessorType::Source, ProcessorType::Source);
        assert_ne!(ProcessorType::Source, ProcessorType::Sink);
        assert_ne!(ProcessorType::Sink, ProcessorType::Transform);
    }

    #[test]
    fn test_processor_type_debug() {
        let source_type = ProcessorType::Source;
        let debug_str = format!("{:?}", source_type);
        assert_eq!(debug_str, "Source");
    }
}
