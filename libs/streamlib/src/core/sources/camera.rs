
use crate::core::{
    StreamOutput, VideoFrame,
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use crate::core::traits::{StreamElement, StreamProcessor};
use std::sync::Arc;
use serde_json::json;

type Result<T> = std::result::Result<T, crate::StreamError>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CameraConfig {
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

#[derive(Debug, Clone)]
pub struct CameraDevice {
    pub id: String,
    pub name: String,
}

pub struct CameraOutputPorts {
    pub video: StreamOutput<VideoFrame>,
}

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

pub trait CameraProcessor: StreamElement + StreamProcessor<Config = CameraConfig> {
    fn set_device_id(&mut self, device_id: &str) -> Result<()>;

    fn list_devices() -> Result<Vec<CameraDevice>>;

    fn output_ports(&mut self) -> &mut CameraOutputPorts;
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ProcessorDescriptor, SCHEMA_VIDEO_FRAME};

    #[test]
    fn test_camera_descriptor_helper() {
        let desc = descriptor();

        assert_eq!(desc.name, "CameraProcessor");
        assert!(desc.description.contains("camera"));
        assert!(desc.usage_context.is_some());

        assert_eq!(desc.inputs.len(), 0);

        assert_eq!(desc.outputs.len(), 1);
        assert_eq!(desc.outputs[0].name, "video");
        assert_eq!(desc.outputs[0].schema.name, "VideoFrame");
        assert!(desc.outputs[0].required);

        assert!(desc.tags.contains(&"source".to_string()));
        assert!(desc.tags.contains(&"camera".to_string()));
    }

    #[test]
    fn test_camera_descriptor_serialization() {
        let desc = descriptor();

        let json = desc.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("CameraProcessor"));
        assert!(json.contains("video"));
        assert!(json.contains("VideoFrame"));
    }

    #[test]
    fn test_video_frame_schema() {
        let schema = &*SCHEMA_VIDEO_FRAME;

        assert_eq!(schema.name, "VideoFrame");
        assert_eq!(schema.version.major, 1);
        assert_eq!(schema.version.minor, 0);

        let field_names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"texture"));
        assert!(field_names.contains(&"width"));
        assert!(field_names.contains(&"height"));
        assert!(field_names.contains(&"timestamp"));
        assert!(field_names.contains(&"frame_number"));
    }

    #[test]
    fn test_camera_config_default() {
        let config = CameraConfig::default();
        assert!(config.device_id.is_none());
    }

    #[test]
    fn test_camera_config_from_unit() {
        let config: CameraConfig = ().into();
        assert!(config.device_id.is_none());
    }
}
