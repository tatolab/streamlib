//! Video frame message type
//!
//! Represents a single frame of video data as a WebGPU texture.
//! Platform-specific processors convert their native GPU types internally.

use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;

/// Video frame message
///
/// Represents a single frame of video data as a WebGPU texture.
/// Platform-specific processors convert their native GPU types internally.
///
/// # Architecture
///
/// - Camera processors (streamlib-apple): Metal → WebGPU
/// - Display processors (streamlib-apple): WebGPU → Metal (if needed)
/// - Effect processors: Work directly with WebGPU shaders
///
/// # Example
///
/// ```ignore
/// use streamlib::VideoFrame;
///
/// // In a processor
/// let frame = video_input.read_latest().unwrap();
/// println!("Frame {}x{} @ {:.3}s",
///     frame.width, frame.height, frame.timestamp);
///
/// // Access WebGPU texture directly
/// let texture: &wgpu::Texture = &frame.texture;
/// ```
#[derive(Clone)]
pub struct VideoFrame {
    /// WebGPU texture containing the frame data
    ///
    /// This is the universal GPU representation. Platform processors
    /// convert to/from their native types (Metal, Vulkan) internally.
    pub texture: Arc<wgpu::Texture>,

    /// Texture format (Rgba8Unorm or Bgra8Unorm)
    ///
    /// Cached for performance. Should always match `texture.format()`.
    /// - **Internal pipeline standard**: Rgba8Unorm (used by most processors)
    /// - **Platform edges**: Bgra8Unorm (camera input, display output on some platforms)
    pub format: wgpu::TextureFormat,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Sequential frame number
    pub frame_number: u64,

    /// Frame width in pixels
    pub width: u32,

    /// Frame height in pixels
    pub height: u32,

    /// Optional metadata (detection boxes, ML results, etc.)
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl VideoFrame {
    /// Create a new video frame
    pub fn new(
        texture: Arc<wgpu::Texture>,
        format: wgpu::TextureFormat,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp,
            frame_number,
            width,
            height,
            metadata: None,
        }
    }

    /// Create a video frame with metadata
    pub fn with_metadata(
        texture: Arc<wgpu::Texture>,
        format: wgpu::TextureFormat,
        timestamp: f64,
        frame_number: u64,
        width: u32,
        height: u32,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp,
            frame_number,
            width,
            height,
            metadata: Some(metadata),
        }
    }

    /// Create example 720p video frame metadata for MCP/macro use
    ///
    /// Returns a JSON representation suitable for ProcessorExample.
    /// Note: This is metadata only (no actual texture), used by MCP for documentation.
    pub fn example_720p() -> serde_json::Value {
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example 1080p video frame metadata for MCP/macro use
    pub fn example_1080p() -> serde_json::Value {
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }

    /// Create example 4K video frame metadata for MCP/macro use
    pub fn example_4k() -> serde_json::Value {
        serde_json::json!({
            "width": 3840,
            "height": 2160,
            "format": "Rgba8Unorm",
            "timestamp": 0.033,
            "frame_number": 1,
            "metadata": {}
        })
    }
}

impl PortMessage for VideoFrame {
    fn port_type() -> PortType {
        PortType::Video
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_VIDEO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("720p video", Self::example_720p()),
            ("1080p video", Self::example_1080p()),
            ("4K video", Self::example_4k()),
        ]
    }
}
