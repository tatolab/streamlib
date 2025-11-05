use parking_lot::Mutex;
use std::sync::Arc;

use super::runtime::WakeupEvent;
use super::connection::ProcessorConnection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    Video,
    Audio1,
    Audio2,
    Audio4,
    Audio6,
    Audio8,
    Data,
}

pub trait PortMessage: Clone + Send + 'static {
    fn port_type() -> PortType;
    fn schema() -> std::sync::Arc<crate::core::Schema>;
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }
}

impl PortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            PortType::Video => 3,
            PortType::Audio1 | PortType::Audio2 | PortType::Audio4 | PortType::Audio6 | PortType::Audio8 => 4,
            PortType::Data => 16,
        }
    }

    pub fn compatible_with(&self, other: &PortType) -> bool {
        self == other
    }
}

pub struct StreamOutput<T: PortMessage> {
    name: String,
    port_type: PortType,
    connections: Arc<Mutex<Vec<Arc<ProcessorConnection<T>>>>>,
    downstream_wakeup: Mutex<Option<crossbeam_channel::Sender<WakeupEvent>>>,
}

impl<T: PortMessage> StreamOutput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            connections: Arc::new(Mutex::new(Vec::new())),
            downstream_wakeup: Mutex::new(None),
        }
    }

    pub fn write(&self, data: T) {
        let connections = self.connections.lock();
        for conn in connections.iter() {
            let _ = conn.write(data.clone());
        }

        if !connections.is_empty() {
            if let Some(wakeup_tx) = self.downstream_wakeup.lock().as_ref() {
                let _ = wakeup_tx.send(WakeupEvent::DataAvailable);
            }
        }
    }

    pub fn add_connection(&self, connection: Arc<ProcessorConnection<T>>) {
        self.connections.lock().push(connection);
    }

    pub fn connections(&self) -> Vec<Arc<ProcessorConnection<T>>> {
        self.connections.lock().clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }

    pub fn set_downstream_wakeup(&self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        *self.downstream_wakeup.lock() = Some(wakeup_tx);
    }
}

impl<T: PortMessage> Clone for StreamOutput<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            connections: Arc::clone(&self.connections),
            downstream_wakeup: Mutex::new(self.downstream_wakeup.lock().clone()),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOutput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .finish()
    }
}

pub struct StreamInput<T: PortMessage> {
    name: String,
    port_type: PortType,
    connection: Mutex<Option<Arc<ProcessorConnection<T>>>>,
}

impl<T: PortMessage> StreamInput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            connection: Mutex::new(None),
        }
    }

    pub fn set_connection(&self, connection: Arc<ProcessorConnection<T>>) {
        *self.connection.lock() = Some(connection);
    }

    pub fn read_latest(&self) -> Option<T> {
        self.connection.lock().as_ref()?.read_latest()
    }

    pub fn read_all(&self) -> Vec<T> {
        if let Some(conn) = self.connection.lock().as_ref() {
            let mut items = Vec::new();
            while let Some(item) = conn.read_latest() {
                items.push(item);
            }
            items
        } else {
            Vec::new()
        }
    }

    pub fn has_data(&self) -> bool {
        self.connection.lock()
            .as_ref()
            .map(|conn| conn.has_data())
            .unwrap_or(false)
    }

    pub fn is_connected(&self) -> bool {
        self.connection.lock().is_some()
    }

    pub fn clone_connection(&self) -> Option<Arc<ProcessorConnection<T>>> {
        self.connection.lock().as_ref().map(Arc::clone)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }
}

impl<T: PortMessage> Clone for StreamInput<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            connection: Mutex::new(self.connection.lock().clone()),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamInput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .field("connected", &self.is_connected())
            .finish()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    impl PortMessage for i32 {
        fn port_type() -> PortType {
            PortType::Data
        }

        fn schema() -> std::sync::Arc<crate::core::Schema> {
            use crate::core::{Schema, Field, FieldType, SemanticVersion, SerializationFormat};
            std::sync::Arc::new(
                Schema::new(
                    "i32",
                    SemanticVersion::new(1, 0, 0),
                    vec![Field::new("value", FieldType::Int32)],
                    SerializationFormat::Bincode,
                )
            )
        }
    }

    #[test]
    fn test_port_type_defaults() {
        assert_eq!(PortType::Video.default_capacity(), 3);
        assert_eq!(PortType::Audio.default_capacity(), 3);
        assert_eq!(PortType::Data.default_capacity(), 16);
    }

    #[test]
    fn test_output_creation() {
        let output = StreamOutput::<i32>::new("test");
        assert_eq!(output.name(), "test");
        assert_eq!(output.port_type(), PortType::Data);
    }

    #[test]
    fn test_input_creation() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.name(), "test");
        assert_eq!(input.port_type(), PortType::Data);
        assert!(!input.is_connected());
    }

    #[test]
    fn test_write_and_read() {
        let output = StreamOutput::<i32>::new("output");
        let input = StreamInput::<i32>::new("input");

        let connection = Arc::new(ProcessorConnection::new(
            "source".to_string(),
            "output".to_string(),
            "dest".to_string(),
            "input".to_string(),
            4,  // capacity
        ));

        output.add_connection(Arc::clone(&connection));
        input.set_connection(Arc::clone(&connection));

        assert!(input.is_connected());

        output.write(42);
        output.write(100);

        assert_eq!(input.read_latest(), Some(100));
    }

    #[test]
    fn test_fan_out() {
        let output = StreamOutput::<i32>::new("output");
        let input1 = StreamInput::<i32>::new("input1");
        let input2 = StreamInput::<i32>::new("input2");

        let conn1 = Arc::new(ProcessorConnection::new(
            "source".to_string(),
            "output".to_string(),
            "dest1".to_string(),
            "input1".to_string(),
            4,
        ));

        let conn2 = Arc::new(ProcessorConnection::new(
            "source".to_string(),
            "output".to_string(),
            "dest2".to_string(),
            "input2".to_string(),
            4,
        ));

        output.add_connection(Arc::clone(&conn1));
        output.add_connection(Arc::clone(&conn2));
        input1.set_connection(conn1);
        input2.set_connection(conn2);

        output.write(42);

        assert_eq!(input1.read_latest(), Some(42));
        assert_eq!(input2.read_latest(), Some(42));
    }

    #[test]
    fn test_read_all() {
        let output = StreamOutput::<i32>::new("output");
        let input = StreamInput::<i32>::new("input");

        let connection = Arc::new(ProcessorConnection::new(
            "source".to_string(),
            "output".to_string(),
            "dest".to_string(),
            "input".to_string(),
            4,
        ));

        output.add_connection(Arc::clone(&connection));
        input.set_connection(connection);

        output.write(1);
        output.write(2);
        output.write(3);

        let data = input.read_all();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], 3);

        let data2 = input.read_all();
        assert_eq!(data2.len(), 0);
    }

    #[test]
    fn test_read_from_unconnected() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.read_latest(), None);
        assert_eq!(input.read_all().len(), 0);
    }
}
