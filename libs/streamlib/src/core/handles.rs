use super::PortMessage;
use std::marker::PhantomData;

pub type ProcessorId = String;

#[derive(Debug, Clone)]
pub struct ProcessorHandle {
    pub(crate) id: ProcessorId,
}

impl ProcessorHandle {
    pub(crate) fn new(id: ProcessorId) -> Self {
        Self { id }
    }

    pub fn id(&self) -> &ProcessorId {
        &self.id
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
