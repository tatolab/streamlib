//! StreamElement - Base trait for all processors
//!
//! Inspired by GStreamer's GstElement, this provides a common interface for
//! all processor types (sources, sinks, transforms).
//!
//! ## Design Philosophy
//!
//! Following GStreamer's architecture, all processors in streamlib inherit from
//! a base `StreamElement` trait that provides:
//!
//! - **Lifecycle management**: start(), stop(), shutdown()
//! - **Introspection**: Query ports and capabilities
//! - **Type-safe downcasting**: Convert to specialized trait types
//! - **Uniform runtime handling**: Single storage collection
//!
//! ## Type Hierarchy
//!
//! ```text
//! StreamElement (base trait)
//!     ├─ StreamSource (no inputs, only outputs)
//!     ├─ StreamSink (only inputs, no outputs)
//!     └─ StreamTransform (any I/O configuration)
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use streamlib::core::traits::{StreamElement, ElementType};
//!
//! fn process_element(element: &dyn StreamElement) {
//!     match element.element_type() {
//!         ElementType::Source => {
//!             if let Some(source) = element.as_source() {
//!                 // Handle source-specific logic
//!             }
//!         }
//!         ElementType::Sink => {
//!             if let Some(sink) = element.as_sink() {
//!                 // Handle sink-specific logic
//!             }
//!         }
//!         ElementType::Transform => {
//!             if let Some(transform) = element.as_transform() {
//!                 // Handle transform-specific logic
//!             }
//!         }
//!     }
//! }
//! ```

use crate::core::schema::{ProcessorDescriptor, PortDescriptor};
use crate::core::error::Result;
use crate::core::RuntimeContext;
use serde::{Deserialize, Serialize};

pub trait ProcessorConfig: Serialize + for<'de> Deserialize<'de> {
    type Processor: StreamElement;
    fn build(self) -> Result<Self::Processor>;
}

/// Type of stream element
///
/// Determines the processor's role in the pipeline:
/// - **Source**: Data generator (cameras, mics, test signals)
/// - **Sink**: Data consumer (displays, speakers, file writers)
/// - **Transform**: Data processor (effects, mixers, analyzers)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    /// Source element - no inputs, only outputs
    Source,

    /// Sink element - only inputs, no outputs
    Sink,

    /// Transform element - processes data (any I/O configuration)
    Transform,
}

/// Base trait for all stream processors
///
/// Provides common functionality that all processors share, regardless of type.
/// Inspired by GStreamer's GstElement base class.
///
/// ## Lifecycle
///
/// All elements follow this lifecycle:
///
/// 1. **Construction**: Created via `from_config()`
/// 2. **Start**: Resources allocated, connections verified
/// 3. **Processing**: Data flows through the pipeline
/// 4. **Stop**: Graceful pause (can be restarted)
/// 5. **Shutdown**: Final cleanup, resources released
///
/// ## Introspection
///
/// Elements can be queried for their capabilities:
/// - Port structure (input/output ports)
/// - Processor descriptor (metadata, tags, examples)
/// - Element type (source/sink/transform)
///
/// ## Type-Safe Downcasting
///
/// Use the `as_*()` methods to safely convert to specialized traits:
///
/// ```rust,ignore
/// if let Some(source) = element.as_source() {
///     let frame = source.generate()?;
/// }
/// ```
pub trait StreamElement: Send + 'static {
    /// Returns the name of this element instance
    ///
    /// Used for debugging, logging, and user-facing identification.
    /// Should be unique within a runtime instance.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// assert_eq!(camera.name(), "camera_0");
    /// assert_eq!(display.name(), "display_0");
    /// ```
    fn name(&self) -> &str;

    /// Returns the type of this element
    ///
    /// Determines how the runtime schedules and executes this processor:
    /// - **Source**: Scheduled in loop, generates data
    /// - **Sink**: Receives data, may provide clock
    /// - **Transform**: Purely reactive to data arrival
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// match element.element_type() {
    ///     ElementType::Source => println!("This is a data generator"),
    ///     ElementType::Sink => println!("This is a data consumer"),
    ///     ElementType::Transform => println!("This is a data processor"),
    /// }
    /// ```
    fn element_type(&self) -> ElementType;

    /// Returns the descriptor for this processor type
    ///
    /// Provides metadata for AI/MCP discoverability, including:
    /// - Human-readable description
    /// - Input/output port schemas
    /// - Configuration parameters
    /// - Tags for categorization
    /// - Usage examples
    ///
    /// Returns `None` for internal/helper processors that shouldn't be
    /// exposed to AI agents.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(desc) = element.descriptor() {
    ///     println!("Processor: {}", desc.name);
    ///     println!("Description: {}", desc.description);
    /// }
    /// ```
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    // ============================================================
    // Lifecycle Methods
    // ============================================================

    /// Start the element
    ///
    /// Called when the runtime starts. Allocate resources, verify connections,
    /// and prepare for processing.
    ///
    /// The runtime context provides access to shared resources:
    /// - GPU context (device + queue)
    /// - Future: clocks, allocators, buffer pools, etc.
    ///
    /// Elements can store whatever they need from the context in their fields.
    /// This follows GStreamer's GstContext pattern.
    ///
    /// Default implementation does nothing (stateless processors).
    ///
    /// # Arguments
    ///
    /// * `ctx` - Runtime context providing access to shared resources
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Resource allocation fails (GPU memory, hardware devices)
    /// - Required connections are missing
    /// - Configuration is invalid
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
    ///     // Store GPU context for later use
    ///     self.gpu_context = Some(ctx.gpu.clone());
    ///
    ///     // Initialize hardware
    ///     self.device = open_camera_device()?;
    ///     Ok(())
    /// }
    /// ```
    fn start(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    /// Stop the element
    ///
    /// Called when the runtime stops. Release resources, but maintain
    /// configuration so the element can be restarted.
    ///
    /// Default implementation does nothing.
    ///
    /// # Errors
    ///
    /// Returns error if resource cleanup fails (non-fatal).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn stop(&mut self) -> Result<()> {
    ///     self.device.close()?;
    ///     self.device = None;
    ///     Ok(())
    /// }
    /// ```
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    /// Shutdown the element permanently
    ///
    /// Called when the element is being removed from the runtime.
    /// Release all resources, including configuration.
    ///
    /// Default implementation calls `stop()`.
    ///
    /// # Errors
    ///
    /// Returns error if final cleanup fails (logged, not fatal).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn shutdown(&mut self) -> Result<()> {
    ///     self.stop()?;
    ///     self.config.clear();
    ///     Ok(())
    /// }
    /// ```
    fn shutdown(&mut self) -> Result<()> {
        self.stop()
    }

    // ============================================================
    // Introspection Methods
    // ============================================================

    /// Returns descriptors for all input ports
    ///
    /// Used for:
    /// - Runtime connection validation
    /// - AI/MCP discovery
    /// - Debug visualization
    ///
    /// Default returns empty vector (sources have no inputs).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let inputs = element.input_ports();
    /// for port in inputs {
    ///     println!("Input: {} ({})", port.name, port.data_type);
    /// }
    /// ```
    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    /// Returns descriptors for all output ports
    ///
    /// Used for:
    /// - Runtime connection validation
    /// - AI/MCP discovery
    /// - Debug visualization
    ///
    /// Default returns empty vector (sinks have no outputs).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let outputs = element.output_ports();
    /// for port in outputs {
    ///     println!("Output: {} ({})", port.name, port.data_type);
    /// }
    /// ```
    fn output_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    // ============================================================
    // Type-Safe Downcasting
    // ============================================================

    /// Try to downcast to StreamSource
    ///
    /// Returns `Some` if this element is a source, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Sources should override this to return Some(self).
    fn as_source(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Try to downcast to StreamSource (mutable)
    ///
    /// Returns `Some` if this element is a source, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Sources should override this to return Some(self).
    fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Try to downcast to StreamSink
    ///
    /// Returns `Some` if this element is a sink, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Sinks should override this to return Some(self).
    fn as_sink(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Try to downcast to StreamSink (mutable)
    ///
    /// Returns `Some` if this element is a sink, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Sinks should override this to return Some(self).
    fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Try to downcast to StreamTransform
    ///
    /// Returns `Some` if this element is a transform, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Transforms should override this to return Some(self).
    fn as_transform(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Try to downcast to StreamTransform (mutable)
    ///
    /// Returns `Some` if this element is a transform, `None` otherwise.
    ///
    /// Note: This is a placeholder. Actual downcasting is done via std::any::Any.
    /// Transforms should override this to return Some(self).
    fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::source::StreamSource;

    /// Mock source for testing
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

    // MockSource intentionally does not implement StreamSource
    // Tests only check StreamElement functionality

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

        // Default implementations should not error
        // Note: start() requires RuntimeContext, tested in integration tests
        assert!(source.stop().is_ok());
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
