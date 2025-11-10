
use super::metadata::MetadataValue;
use crate::core::bus::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;

// Implement sealed trait
impl crate::core::bus::ports::sealed::Sealed for DataFrame {}

#[derive(Clone)]
pub struct DataFrame {
    pub buffer: Arc<wgpu::Buffer>,

    pub timestamp: f64,

    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl DataFrame {
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
