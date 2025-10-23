//! Camera processor trait
//!
//! Defines the interface for camera capture processors across platforms.

use crate::{StreamProcessor, StreamOutput, VideoFrame};

// Re-import Result type for trait methods
type Result<T> = std::result::Result<T, crate::StreamError>;

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
