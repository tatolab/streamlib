
use crate::core::{
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use std::sync::Arc;
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub title: Option<String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            title: None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

// Ports are now defined directly on platform-specific implementations
// No shared port struct needed - each implementation uses #[derive(StreamProcessor)]

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

pub trait DisplayProcessor {
    fn set_window_title(&mut self, title: &str);

    fn window_id(&self) -> Option<WindowId>;
}



#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::{ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME};
    use std::sync::Arc;

    struct MockDisplayProcessor;

    impl StreamProcessor for MockDisplayProcessor {
        type Config = crate::core::EmptyConfig;

        fn from_config(_config: Self::Config) -> crate::Result<Self> {
            Ok(Self)
        }

        fn process(&mut self) -> crate::Result<()> {
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
    }

    #[test]
    fn test_display_descriptor() {
        let descriptor = MockDisplayProcessor::descriptor().expect("Should have descriptor");

        assert_eq!(descriptor.name, "DisplayProcessor");
        assert!(descriptor.description.contains("window"));
        assert!(descriptor.usage_context.is_some());

        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(descriptor.inputs[0].name, "video");
        assert_eq!(descriptor.inputs[0].schema.name, "VideoFrame");
        assert!(descriptor.inputs[0].required);

        assert_eq!(descriptor.outputs.len(), 0);

        assert!(descriptor.tags.contains(&"sink".to_string()));
        assert!(descriptor.tags.contains(&"display".to_string()));
    }

    #[test]
    fn test_display_descriptor_serialization() {
        let descriptor = MockDisplayProcessor::descriptor().expect("Should have descriptor");

        let json = descriptor.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("DisplayProcessor"));
        assert!(json.contains("video"));
        assert!(json.contains("VideoFrame"));

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
