//! Camera processor trait
//!
//! Defines the interface for camera capture processors across platforms.

use crate::core::{
    StreamProcessor, StreamOutput, VideoFrame,
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use std::sync::Arc;
use serde_json::json;

// Re-import Result type for trait methods
type Result<T> = std::result::Result<T, crate::StreamError>;

/// Configuration for camera processors
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CameraConfig {
    /// Optional device ID to use (e.g., "0x1234" on macOS)
    /// If None, uses the default camera
    pub device_id: Option<String>,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self { device_id: None }
    }
}

impl From<()> for CameraConfig {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

/// Information about an available camera device
#[derive(Debug, Clone)]
pub struct CameraDevice {
    /// Unique device identifier
    pub id: String,
    /// Human-readable device name
    pub name: String,
}

/// Output ports for camera processors
pub struct CameraOutputPorts {
    /// Video frame output (WebGPU textures)
    pub video: StreamOutput<VideoFrame>,
}

/// Get the standard descriptor for camera processors
///
/// Platform implementations should use this descriptor in their
/// `StreamProcessor::descriptor()` implementation unless they need
/// to add platform-specific information.
pub fn descriptor() -> ProcessorDescriptor {
    ProcessorDescriptor::new(
        "CameraProcessor",
        "Captures video frames from a camera device. Outputs WebGPU textures at the configured frame rate."
    )
    .with_usage_context(
        "Use when you need live video input from a camera. This is typically the source \
         processor in a pipeline. Supports multiple camera devices - use set_device_id() \
         to select a specific camera, or use 'default' for the system default camera."
    )
    .with_output(PortDescriptor::new(
        "video",
        Arc::clone(&SCHEMA_VIDEO_FRAME),
        true,
        "Live video frames from the camera. Each frame is a WebGPU texture with timestamp \
         and metadata. Frames are produced at the camera's native frame rate (typically 30 or 60 FPS)."
    ))
    .with_example(ProcessorExample::new(
        "720p video capture at 30 FPS",
        json!({}),  // No inputs (source processor)
        json!({
            "video": {
                "width": 1280,
                "height": 720,
                "format": "RGBA8",
                "timestamp": 0.033,
                "frame_number": 1,
                "metadata": {}
            }
        })
    ))
    .with_example(ProcessorExample::new(
        "1080p video capture at 60 FPS",
        json!({}),  // No inputs (source processor)
        json!({
            "video": {
                "width": 1920,
                "height": 1080,
                "format": "RGBA8",
                "timestamp": 0.016,
                "frame_number": 1,
                "metadata": {}
            }
        })
    ))
    .with_tags(vec!["source", "camera", "video", "input", "capture"])
}

/// Camera processor trait
///
/// Platform implementations (AppleCameraProcessor, etc.) implement this trait
/// to provide camera capture functionality with WebGPU texture output.
pub trait CameraProcessor: StreamProcessor {
    /// Set the camera device to use
    ///
    /// # Arguments
    /// * `device_id` - Device ID from `list_devices()`, or "default" for default camera
    fn set_device_id(&mut self, device_id: &str) -> Result<()>;

    /// List available camera devices
    fn list_devices() -> Result<Vec<CameraDevice>>;

    /// Get the output ports for this camera
    fn output_ports(&mut self) -> &mut CameraOutputPorts;
}

// NOTE: Port access methods are now part of StreamProcessor trait.
// Concrete camera processor types (AppleCameraProcessor, etc.) should override
// the with_video_output_mut() method from StreamProcessor to provide access to their "video" output port.

// NOTE: Descriptor-only registration removed - platform implementations (AppleCameraProcessor, etc.)
// register themselves with full factory support via register_processor_type! macro.
// The facade in lib.rs re-exports them as CameraProcessor/DisplayProcessor.

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::{ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME};
    use std::sync::Arc;

    // Mock implementation for testing
    struct MockCameraProcessor;

    impl StreamProcessor for MockCameraProcessor {
        type Config = crate::core::EmptyConfig;

        fn from_config(_config: Self::Config) -> Result<Self> {
            Ok(Self)
        }

        fn process(&mut self) -> Result<()> {
            Ok(())
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            Some(
                ProcessorDescriptor::new(
                    "CameraProcessor",
                    "Captures video frames from a camera device. Outputs WebGPU textures at the configured frame rate."
                )
                .with_usage_context(
                    "Use when you need live video input from a camera. This is typically the source \
                     processor in a pipeline. Supports multiple camera devices - use set_device_id() \
                     to select a specific camera, or use 'default' for the system default camera."
                )
                .with_output(PortDescriptor::new(
                    "video",
                    Arc::clone(&SCHEMA_VIDEO_FRAME),
                    true,
                    "Live video frames from the camera. Each frame is a WebGPU texture with timestamp \
                     and metadata. Frames are produced at the camera's native frame rate (typically 30 or 60 FPS)."
                ))
                .with_tags(vec!["source", "camera", "video", "input", "capture"])
            )
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    impl CameraProcessor for MockCameraProcessor {
        fn set_device_id(&mut self, _device_id: &str) -> Result<()> {
            Ok(())
        }

        fn list_devices() -> Result<Vec<CameraDevice>> {
            Ok(vec![])
        }

        fn output_ports(&mut self) -> &mut CameraOutputPorts {
            unimplemented!()
        }
    }

    #[test]
    fn test_camera_descriptor() {
        let descriptor = MockCameraProcessor::descriptor().expect("Should have descriptor");

        // Verify basic metadata
        assert_eq!(descriptor.name, "CameraProcessor");
        assert!(descriptor.description.contains("camera"));
        assert!(descriptor.usage_context.is_some());

        // Verify it has no inputs (it's a source)
        assert_eq!(descriptor.inputs.len(), 0);

        // Verify it has video output
        assert_eq!(descriptor.outputs.len(), 1);
        assert_eq!(descriptor.outputs[0].name, "video");
        assert_eq!(descriptor.outputs[0].schema.name, "VideoFrame");
        assert!(descriptor.outputs[0].required);

        // Verify tags
        assert!(descriptor.tags.contains(&"source".to_string()));
        assert!(descriptor.tags.contains(&"camera".to_string()));
    }

    #[test]
    fn test_camera_descriptor_serialization() {
        let descriptor = MockCameraProcessor::descriptor().expect("Should have descriptor");

        // Test JSON serialization
        let json = descriptor.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("CameraProcessor"));
        assert!(json.contains("video"));
        assert!(json.contains("VideoFrame"));

        // Note: YAML serialization not tested due to serde_yaml limitation with nested enums
        // JSON serialization is sufficient for AI agent consumption
    }

    #[test]
    fn test_video_frame_schema() {
        let schema = &*SCHEMA_VIDEO_FRAME;

        // Verify schema structure
        assert_eq!(schema.name, "VideoFrame");
        assert_eq!(schema.version.major, 1);
        assert_eq!(schema.version.minor, 0);

        // Verify required fields exist
        let field_names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"texture"));
        assert!(field_names.contains(&"width"));
        assert!(field_names.contains(&"height"));
        assert!(field_names.contains(&"timestamp"));
        assert!(field_names.contains(&"frame_number"));
    }
}
