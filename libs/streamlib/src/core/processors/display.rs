//! Display processor trait
//!
//! Defines the interface for display/window processors across platforms.

use crate::core::{
    StreamProcessor, StreamInput, VideoFrame,
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use std::sync::Arc;
use serde_json::json;

/// Unique identifier for a display window
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

/// Input ports for display processors
pub struct DisplayInputPorts {
    /// Video frame input (WebGPU textures)
    pub video: StreamInput<VideoFrame>,
}

/// Get the standard descriptor for display processors
///
/// Platform implementations should use this descriptor in their
/// `StreamProcessor::descriptor()` implementation unless they need
/// to add platform-specific information.
pub fn descriptor() -> ProcessorDescriptor {
    ProcessorDescriptor::new(
        "DisplayProcessor",
        "Displays video frames in a window. Renders WebGPU textures to the screen at the configured frame rate."
    )
    .with_usage_context(
        "Use when you need to visualize video output in a window. This is typically a sink \
         processor at the end of a pipeline. Each DisplayProcessor manages one window. The window \
         is created automatically on first frame and can be configured with set_window_title()."
    )
    .with_input(PortDescriptor::new(
        "video",
        Arc::clone(&SCHEMA_VIDEO_FRAME),
        true,
        "Video frames to display. Accepts WebGPU textures and renders them to the window. \
         Automatically handles format conversion and scaling to fit the window."
    ))
    .with_example(ProcessorExample::new(
        "Display 720p video in a window",
        json!({
            "video": {
                "width": 1280,
                "height": 720,
                "format": "RGBA8",
                "timestamp": 0.033,
                "frame_number": 1,
                "metadata": {}
            }
        }),
        json!({})  // No outputs (sink processor)
    ))
    .with_example(ProcessorExample::new(
        "Display 1080p video with metadata overlay",
        json!({
            "video": {
                "width": 1920,
                "height": 1080,
                "format": "RGBA8",
                "timestamp": 1.250,
                "frame_number": 75,
                "metadata": {
                    "fps": 60.0,
                    "camera_id": "front"
                }
            }
        }),
        json!({})  // No outputs (sink processor)
    ))
    .with_tags(vec!["sink", "display", "window", "output", "render"])
}

/// Display processor trait
///
/// Platform implementations (AppleDisplayProcessor, etc.) implement this trait
/// to provide window/display functionality with WebGPU texture rendering.
///
/// Each DisplayProcessor instance manages one window.
pub trait DisplayProcessor: StreamProcessor {
    /// Set the window title
    fn set_window_title(&mut self, title: &str);

    /// Get the window ID (if window has been created)
    fn window_id(&self) -> Option<WindowId>;

    /// Get the input ports for this display
    fn input_ports(&mut self) -> &mut DisplayInputPorts;
}

// NOTE: Port access methods are now part of StreamProcessor trait.
// Concrete display processor types (AppleDisplayProcessor, etc.) should override
// the with_video_input_mut() method from StreamProcessor to provide access to their "video" input port.

// NOTE: Descriptor-only registration removed - platform implementations (AppleDisplayProcessor, etc.)
// register themselves with full factory support via register_processor_type! macro.
// The facade in lib.rs re-exports them as CameraProcessor/DisplayProcessor.

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::clock::TimedTick;
    use crate::core::{ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME};
    use std::sync::Arc;

    // Mock implementation for testing
    struct MockDisplayProcessor;

    impl StreamProcessor for MockDisplayProcessor {
        fn process(&mut self, _tick: TimedTick) -> crate::Result<()> {
            Ok(())
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            Some(
                ProcessorDescriptor::new(
                    "DisplayProcessor",
                    "Displays video frames in a window. Renders WebGPU textures to the screen at the configured frame rate."
                )
                .with_usage_context(
                    "Use when you need to visualize video output in a window. This is typically a sink \
                     processor at the end of a pipeline. Each DisplayProcessor manages one window. The window \
                     is created automatically on first frame and can be configured with set_window_title()."
                )
                .with_input(PortDescriptor::new(
                    "video",
                    Arc::clone(&SCHEMA_VIDEO_FRAME),
                    true,
                    "Video frames to display. Accepts WebGPU textures and renders them to the window. \
                     Automatically handles format conversion and scaling to fit the window."
                ))
                .with_tags(vec!["sink", "display", "window", "output", "render"])
            )
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    impl DisplayProcessor for MockDisplayProcessor {
        fn set_window_title(&mut self, _title: &str) {}

        fn window_id(&self) -> Option<WindowId> {
            None
        }

        fn input_ports(&mut self) -> &mut DisplayInputPorts {
            unimplemented!()
        }
    }

    #[test]
    fn test_display_descriptor() {
        let descriptor = MockDisplayProcessor::descriptor().expect("Should have descriptor");

        // Verify basic metadata
        assert_eq!(descriptor.name, "DisplayProcessor");
        assert!(descriptor.description.contains("window"));
        assert!(descriptor.usage_context.is_some());

        // Verify it has video input
        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(descriptor.inputs[0].name, "video");
        assert_eq!(descriptor.inputs[0].schema.name, "VideoFrame");
        assert!(descriptor.inputs[0].required);

        // Verify it has no outputs (it's a sink)
        assert_eq!(descriptor.outputs.len(), 0);

        // Verify tags
        assert!(descriptor.tags.contains(&"sink".to_string()));
        assert!(descriptor.tags.contains(&"display".to_string()));
    }

    #[test]
    fn test_display_descriptor_serialization() {
        let descriptor = MockDisplayProcessor::descriptor().expect("Should have descriptor");

        // Test JSON serialization
        let json = descriptor.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("DisplayProcessor"));
        assert!(json.contains("video"));
        assert!(json.contains("VideoFrame"));

        // Note: YAML serialization not tested due to serde_yaml limitation with nested enums
        // JSON serialization is sufficient for AI agent consumption
    }

    #[test]
    fn test_window_id() {
        let id1 = WindowId(42);
        let id2 = WindowId(42);
        let id3 = WindowId(99);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }
}
