use super::metadata::MetadataValue;
use crate::core::link_channel::{LinkPortMessage, LinkPortType};
use std::collections::HashMap;
use std::sync::Arc;

// Implement sealed trait
impl crate::core::link_channel::link_ports::sealed::Sealed for DataFrame {}

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

impl LinkPortMessage for DataFrame {
    fn port_type() -> LinkPortType {
        LinkPortType::Data
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE)
    }
}
