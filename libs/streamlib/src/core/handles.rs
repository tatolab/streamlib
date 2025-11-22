use super::PortMessage;
use std::marker::PhantomData;

pub type ProcessorId = String;

/// Handle to a processor in the runtime
///
/// Provides access to processor metadata including ID, type name, and optional
/// config checksum for graph optimization and caching.
#[derive(Debug, Clone)]
pub struct ProcessorHandle {
    pub(crate) id: ProcessorId,
    /// Processor type name (e.g., "streamlib::core::processors::CameraProcessor")
    processor_type: String,
    /// Optional checksum of processor config (for cache keys)
    config_checksum: Option<u64>,
}

impl ProcessorHandle {
    /// Create a basic ProcessorHandle (used in tests)
    #[allow(dead_code)]
    pub(crate) fn new(id: ProcessorId) -> Self {
        Self {
            id,
            processor_type: String::from("unknown"),
            config_checksum: None,
        }
    }

    /// Create a new ProcessorHandle with metadata
    pub(crate) fn with_metadata(
        id: ProcessorId,
        processor_type: String,
        config_checksum: Option<u64>,
    ) -> Self {
        Self {
            id,
            processor_type,
            config_checksum,
        }
    }

    pub fn id(&self) -> &ProcessorId {
        &self.id
    }

    /// Get the processor type name
    pub fn processor_type(&self) -> &str {
        &self.processor_type
    }

    /// Get the optional config checksum (for caching)
    pub fn config_checksum(&self) -> Option<u64> {
        self.config_checksum
    }

    pub fn output_port<T: PortMessage>(&self, name: &str) -> OutputPortRef<T> {
        OutputPortRef {
            processor_id: self.id.clone(),
            port_name: name.to_string(),
            _phantom: PhantomData,
        }
    }

    pub fn input_port<T: PortMessage>(&self, name: &str) -> InputPortRef<T> {
        InputPortRef {
            processor_id: self.id.clone(),
            port_name: name.to_string(),
            _phantom: PhantomData,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputPortRef<T: PortMessage> {
    pub(crate) processor_id: ProcessorId,
    pub(crate) port_name: String,
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> OutputPortRef<T> {
    pub fn processor_id(&self) -> &ProcessorId {
        &self.processor_id
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

#[derive(Debug, Clone)]
pub struct InputPortRef<T: PortMessage> {
    pub(crate) processor_id: ProcessorId,
    pub(crate) port_name: String,
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> InputPortRef<T> {
    pub fn processor_id(&self) -> &ProcessorId {
        &self.processor_id
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingConnection {
    pub id: String,
    pub source_processor_id: ProcessorId,
    pub source_port_name: String,
    pub dest_processor_id: ProcessorId,
    pub dest_port_name: String,
}

impl PendingConnection {
    pub fn new(
        id: String,
        source_processor_id: ProcessorId,
        source_port_name: String,
        dest_processor_id: ProcessorId,
        dest_port_name: String,
    ) -> Self {
        Self {
            id,
            source_processor_id,
            source_port_name,
            dest_processor_id,
            dest_port_name,
        }
    }
}
