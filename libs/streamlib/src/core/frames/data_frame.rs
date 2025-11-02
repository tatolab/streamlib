//! Generic data frame type
//!
//! For custom data types that don't fit VideoFrame or AudioFrame.
//! Uses WebGPU buffer for GPU-resident data.

use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;

/// Generic data frame
///
/// For custom data types that don't fit VideoFrame or AudioFrame.
/// Uses WebGPU buffer for GPU-resident data.
///
/// # Example
///
/// ```ignore
/// use streamlib::DataFrame;
///
/// // ML detection results in GPU buffer
/// let detections = DataFrame::new(
///     detection_buffer,
///     timestamp,
///     Some(hashmap!{ "model".into() => "yolov8".into() })
/// );
/// ```
#[derive(Clone)]
pub struct DataFrame {
    /// WebGPU buffer containing custom data
    pub buffer: Arc<wgpu::Buffer>,

    /// Timestamp in seconds since stream start
    pub timestamp: f64,

    /// Optional metadata
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl DataFrame {
    /// Create a new data frame
    pub fn new(
        buffer: Arc<wgpu::Buffer>,
        timestamp: f64,
        metadata: Option<HashMap<String, MetadataValue>>,
    ) -> Self {
        Self {
            buffer,
            timestamp,
            metadata,
        }
    }
}

impl PortMessage for DataFrame {
    fn port_type() -> PortType {
        PortType::Data
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE)
    }
}
