use parking_lot::Mutex;
use std::sync::Arc;

use super::runtime::WakeupEvent;
use super::connection::ProcessorConnection;

/// Strongly-typed port type that carries compile-time information at runtime.
///
/// Each variant encodes the specific frame type including generic parameters.
/// This allows the Runtime to know exactly what type of connection to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    /// Video frame port
    Video,
    /// Audio frame port with specific channel count
    /// Audio1 = mono, Audio2 = stereo, etc.
    Audio1,
    Audio2,
    Audio4,
    Audio6,
    Audio8,
    /// Generic data message port
    Data,
}

/// Trait for types that can be sent through processor ports.
///
/// This replaces the old BusMessage trait with a simpler interface.
/// Types must be Clone + Send to work with rtrb ring buffers.
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

    /// Check if two port types are compatible for connection.
    pub fn compatible_with(&self, other: &PortType) -> bool {
        self == other
    }
}

/// Output port that writes to multiple downstream connections (fan-out).
///
/// When an output is connected to multiple inputs, we create separate
/// rtrb connections for each, stored in the `connections` vector.
pub struct StreamOutput<T: PortMessage> {
    name: String,
    port_type: PortType,
    /// All rtrb connections from this output (supports fan-out)
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

    /// Write data to all connected downstream ports.
    ///
    /// This implements fan-out: data is pushed to all rtrb connections.
    /// If a connection's ring buffer is full, that write fails silently
    /// (real-time systems drop frames rather than block).
    pub fn write(&self, data: T) {
        let connections = self.connections.lock();
        for conn in connections.iter() {
            // Try to write, ignore errors (buffer full = drop frame)
            let _ = conn.write(data.clone());
        }

        // Wake up downstream processors if configured
        if !connections.is_empty() {
            if let Some(wakeup_tx) = self.downstream_wakeup.lock().as_ref() {
                let _ = wakeup_tx.send(WakeupEvent::DataAvailable);
            }
        }
    }

    /// Add a connection to this output port.
    ///
    /// Called by Runtime when wiring processors together.
    pub fn add_connection(&self, connection: Arc<ProcessorConnection<T>>) {
        self.connections.lock().push(connection);
    }

    /// Get all connections from this output (for introspection).
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

/// Input port that reads from a single upstream connection.
///
/// Each input has exactly one rtrb connection to read from.
pub struct StreamInput<T: PortMessage> {
    name: String,
    port_type: PortType,
    /// Single rtrb connection for this input
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

    /// Connect this input to a ProcessorConnection.
    ///
    /// Called by Runtime when wiring processors together.
    pub fn set_connection(&self, connection: Arc<ProcessorConnection<T>>) {
        *self.connection.lock() = Some(connection);
    }

    /// Read the latest frame from the rtrb ring buffer.
    ///
    /// This drains all frames and returns the most recent one.
    /// Returns None if no connection or no data available.
    pub fn read_latest(&self) -> Option<T> {
        self.connection.lock().as_ref()?.read_latest()
    }

    /// Read all available frames from the ring buffer.
    ///
    /// Drains the buffer and returns all frames in order.
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

    /// Check if there's data available to read.
    pub fn has_data(&self) -> bool {
        self.connection.lock()
            .as_ref()
            .map(|conn| conn.has_data())
            .unwrap_or(false)
    }

    /// Check if this input is connected to an upstream output.
    pub fn is_connected(&self) -> bool {
        self.connection.lock().is_some()
    }

    /// Get a clone of the connection for use in callbacks (e.g., audio output pull pattern).
    ///
    /// Returns None if the input is not connected.
    /// This is useful for scenarios where a callback needs to read data asynchronously.
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

// Old bus helper functions removed - connections now created via Bus and ConnectionManager

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
        // Create ports
        let output = StreamOutput::<i32>::new("output");
        let input = StreamInput::<i32>::new("input");

        // Create connection via ProcessorConnection
        let connection = Arc::new(ProcessorConnection::new(
            "source".to_string(),
            "output".to_string(),
            "dest".to_string(),
            "input".to_string(),
            4,  // capacity
        ));

        // Wire up ports
        output.add_connection(Arc::clone(&connection));
        input.set_connection(Arc::clone(&connection));

        assert!(input.is_connected());

        // Write and read
        output.write(42);
        output.write(100);

        // read_latest drains buffer and returns most recent
        assert_eq!(input.read_latest(), Some(100));
    }

    #[test]
    fn test_fan_out() {
        let output = StreamOutput::<i32>::new("output");
        let input1 = StreamInput::<i32>::new("input1");
        let input2 = StreamInput::<i32>::new("input2");

        // Create separate connections for fan-out
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

        // Wire up ports
        output.add_connection(Arc::clone(&conn1));
        output.add_connection(Arc::clone(&conn2));
        input1.set_connection(conn1);
        input2.set_connection(conn2);

        // Write once
        output.write(42);

        // Both inputs receive the data
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

        // Write multiple values
        output.write(1);
        output.write(2);
        output.write(3);

        // read_latest drains all and returns most recent
        let data = input.read_all();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], 3);

        // Second read returns empty
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
