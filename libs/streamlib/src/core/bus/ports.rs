use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::sync::Arc;

use super::connection::{OwnedConsumer, OwnedProducer};
use super::connection_id::{self, ConnectionId};
use super::connections::{InputConnection, OutputConnection};
use super::plugs::{DisconnectedConsumer, DisconnectedProducer};
use crate::core::bus::WakeupEvent;
use crate::core::graph::ProcessorId;
use crate::core::Result;
use crate::StreamError;
use crossbeam_channel::Sender;

/// Strongly-typed port address combining processor ID and port name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortAddress {
    pub processor_id: ProcessorId,
    pub port_name: Cow<'static, str>,
}

impl PortAddress {
    /// Create a new port address
    pub fn new(processor: impl Into<ProcessorId>, port: impl Into<Cow<'static, str>>) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: port.into(),
        }
    }

    /// Create a port address with a static string port name (zero allocation)
    pub fn with_static(processor: impl Into<ProcessorId>, port: &'static str) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: Cow::Borrowed(port),
        }
    }

    /// Get the full address as "processor_id.port_name"
    pub fn full_address(&self) -> String {
        format!("{}.{}", self.processor_id, self.port_name)
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortType {
    Video,
    Audio,
    Data,
}

/// Sealed trait pattern - only known frame types can implement PortMessage
pub mod sealed {
    pub trait Sealed {}
}

/// Consumption strategy for reading from ports
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumptionStrategy {
    /// Read the latest item, discarding older ones (optimal for video)
    Latest,
    /// Read items sequentially in order (required for audio)
    Sequential,
}

/// Trait for types that can be sent through ports
/// This is a sealed trait - only types in this crate can implement it
pub trait PortMessage: sealed::Sealed + Clone + Send + 'static {
    fn port_type() -> PortType;
    fn schema() -> std::sync::Arc<crate::core::Schema>;
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }

    /// Determines how this type should be consumed from the ring buffer
    /// - Video frames: Latest (skip old frames to show newest)
    /// - Audio frames: Sequential (must play every frame in order)
    fn consumption_strategy() -> ConsumptionStrategy {
        // Default to Latest for backwards compatibility with video
        ConsumptionStrategy::Latest
    }
}

impl PortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            PortType::Video => 3,
            PortType::Audio => 32,
            PortType::Data => 16,
        }
    }

    pub fn compatible_with(&self, other: &PortType) -> bool {
        self == other
    }
}

/// Output port for a processor (always has ≥1 connection, potentially plug)
///
/// Internally uses Arc for sharing and UnsafeCell for interior mutability.
/// This allows ergonomic `&self` methods while maintaining lock-free performance.
pub struct StreamOutput<T: PortMessage> {
    /// Arc-wrapped interior for efficient cloning and sharing
    inner: Arc<StreamOutputInner<T>>,
}

/// Inner implementation of StreamOutput
struct StreamOutputInner<T: PortMessage> {
    /// Connections wrapped in UnsafeCell for interior mutability
    /// SAFETY: Only the owning processor thread ever accesses this
    connections: std::cell::UnsafeCell<Vec<OutputConnection<T>>>,
}

// SAFETY: StreamOutputInner can be safely shared between threads because:
// 1. Each processor thread has exclusive logical ownership of its outputs
// 2. Connections are only modified during setup and disconnect operations
// 3. UnsafeCell is used for interior mutability with manual synchronization guarantee
// 4. OwnedProducer and Sender<WakeupEvent> operations are atomic
unsafe impl<T: PortMessage> Sync for StreamOutputInner<T> {}
unsafe impl<T: PortMessage> Send for StreamOutputInner<T> {}

// Clone just clones the Arc - cheap!
impl<T: PortMessage> Clone for StreamOutput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: PortMessage> StreamOutput<T> {
    /// Create new output with default disconnected plug
    ///
    /// Port name is used to generate unique plug ID
    pub fn new(port_name: &str) -> Self {
        let plug_id =
            connection_id::__private::new_unchecked(format!("{}.disconnected_plug", port_name));
        Self {
            inner: Arc::new(StreamOutputInner {
                connections: std::cell::UnsafeCell::new(vec![OutputConnection::Disconnected {
                    id: plug_id,
                    plug: DisconnectedProducer::new(),
                }]),
            }),
        }
    }

    /// Helper to get mutable access to connections
    /// SAFETY: Caller must ensure exclusive access (processor ownership)
    unsafe fn connections_mut(&self) -> &mut Vec<OutputConnection<T>> {
        &mut *self.inner.connections.get()
    }

    /// Helper to get immutable access to connections
    fn connections(&self) -> &Vec<OutputConnection<T>> {
        unsafe { &*self.inner.connections.get() }
    }

    /// Add a real connection to this output
    ///
    /// # Errors
    /// - Returns error if a connection with this ID already exists
    pub fn add_connection(
        &self,
        connection_id: ConnectionId,
        producer: OwnedProducer<T>,
        wakeup: Sender<WakeupEvent>,
    ) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();

            // Check for duplicate
            if connections.iter().any(|c| c.id() == &connection_id) {
                return Err(StreamError::ConnectionAlreadyExists(
                    connection_id.to_string(),
                ));
            }

            connections.push(OutputConnection::Connected {
                id: connection_id,
                producer,
                wakeup,
            });
        }

        Ok(())
    }

    /// Remove a connection by ID
    ///
    /// # Errors
    /// - Returns error if connection was not found
    ///
    /// Automatically restores plug if last connection removed
    pub fn remove_connection(&self, connection_id: &ConnectionId) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();

            let idx = connections
                .iter()
                .position(|c: &OutputConnection<T>| c.id() == connection_id)
                .ok_or_else(|| StreamError::ConnectionNotFound(connection_id.to_string()))?;

            // Remove the connection
            connections.swap_remove(idx);

            // If no connections left, add plug back
            if connections.is_empty() {
                let plug_id = connection_id::__private::new_unchecked(format!(
                    "{}.disconnected_plug_restored",
                    connection_id
                ));
                connections.push(OutputConnection::Disconnected {
                    id: plug_id,
                    plug: DisconnectedProducer::new(),
                });
            }
        }

        Ok(())
    }

    /// Push data to all connections (including plugs)
    ///
    /// Clones data for all connections except the last (which consumes via move)
    pub fn push(&self, value: T)
    where
        T: Clone,
    {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();

            if connections.is_empty() {
                // Should never happen (we always have plug), but handle gracefully
                tracing::warn!("StreamOutput::push called with no connections (impossible)");
                return;
            }

            // Cache length to avoid borrowing issues
            let len = connections.len();

            // Clone for all except last
            for conn in &mut connections[..len - 1] {
                let _ = conn.push(value.clone());
                conn.wake();
            }

            // Move into last connection
            if let Some(last) = connections.last_mut() {
                let _ = last.push(value);
                last.wake();
            }
        }
    }

    /// Alias for push() - backward compatibility with Phase 2 API
    pub fn write(&self, value: T)
    where
        T: Clone,
    {
        self.push(value)
    }

    /// Check if port has any real connections (not just plugs)
    pub fn is_connected(&self) -> bool {
        self.connections()
            .iter()
            .any(|c: &OutputConnection<T>| c.is_connected())
    }

    /// Get count of real connections (excluding plugs)
    pub fn connection_count(&self) -> usize {
        self.connections()
            .iter()
            .filter(|c: &&OutputConnection<T>| c.is_connected())
            .count()
    }
}

/// Inner state for StreamInput (heap-allocated, wrapped in Arc)
struct StreamInputInner<T: PortMessage> {
    /// Connections (always contains at least one - plug if disconnected)
    /// UnsafeCell allows interior mutability for read operations
    connections: UnsafeCell<Vec<InputConnection<T>>>,
}

/// Input port for a processor (always has ≥1 connection, potentially plug)
///
/// Phase 0.5: Uses interior mutability pattern (UnsafeCell) to allow &self methods.
/// This enables Arc<StreamInput<T>> to work without additional Mutex wrapper.
///
/// SAFETY: Single-threaded processor access makes UnsafeCell safe:
/// - Only one processor thread accesses this port
/// - Runtime manages lifecycle (setup before processing, teardown after)
/// - OwnedConsumer operations are already atomic
pub struct StreamInput<T: PortMessage> {
    /// Arc-wrapped inner state (shared for cloning, UnsafeCell for mutation)
    inner: Arc<StreamInputInner<T>>,
}

// SAFETY: StreamInput can be safely shared between threads because:
// 1. Interior UnsafeCell is only accessed by single processor thread (runtime enforced)
// 2. Arc provides safe sharing across thread boundaries
// 3. OwnedConsumer operations are atomic
unsafe impl<T: PortMessage> Send for StreamInput<T> {}
unsafe impl<T: PortMessage> Sync for StreamInput<T> {}

impl<T: PortMessage> Clone for StreamInput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: PortMessage> StreamInput<T> {
    /// Create new input with default disconnected plug
    ///
    /// Port name is used to generate unique plug ID
    pub fn new(port_name: &str) -> Self {
        let plug_id =
            connection_id::__private::new_unchecked(format!("{}.disconnected_plug", port_name));
        Self {
            inner: Arc::new(StreamInputInner {
                connections: UnsafeCell::new(vec![InputConnection::Disconnected {
                    id: plug_id,
                    plug: DisconnectedConsumer::new(),
                }]),
            }),
        }
    }

    /// SAFETY: Get mutable access to connections
    /// Safe because processor has exclusive ownership (single-threaded access)
    #[inline]
    unsafe fn connections_mut(&self) -> &mut Vec<InputConnection<T>> {
        &mut *self.inner.connections.get()
    }

    /// SAFETY: Get immutable access to connections
    #[inline]
    fn connections(&self) -> &Vec<InputConnection<T>> {
        unsafe { &*self.inner.connections.get() }
    }

    /// Add a real connection to this input
    ///
    /// # Errors
    /// - Returns error if a connection with this ID already exists
    ///
    /// Removes plug if this is the first real connection
    pub fn add_connection(
        &self,
        connection_id: ConnectionId,
        consumer: OwnedConsumer<T>,
        source_address: PortAddress,
        wakeup: Sender<WakeupEvent>,
    ) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();

            // Check for duplicate
            if connections.iter().any(|c| c.id() == &connection_id) {
                return Err(StreamError::ConnectionAlreadyExists(
                    connection_id.to_string(),
                ));
            }

            // Remove plug if this is the first real connection
            if connections.len() == 1 && !connections[0].is_connected() {
                connections.clear();
            }

            connections.push(InputConnection::Connected {
                id: connection_id,
                consumer,
                source_address,
                wakeup,
            });
        }

        Ok(())
    }

    /// Remove a connection by ID
    ///
    /// # Errors
    /// - Returns error if connection was not found
    ///
    /// Automatically restores plug if last connection removed
    pub fn remove_connection(&self, connection_id: &ConnectionId) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();

            let idx = connections
                .iter()
                .position(|c: &InputConnection<T>| c.id() == connection_id)
                .ok_or_else(|| StreamError::ConnectionNotFound(connection_id.to_string()))?;

            connections.swap_remove(idx);

            // If no connections left, add plug back
            if connections.is_empty() {
                let plug_id = connection_id::__private::new_unchecked(format!(
                    "{}.disconnected_plug_restored",
                    connection_id
                ));
                connections.push(InputConnection::Disconnected {
                    id: plug_id,
                    plug: DisconnectedConsumer::new(),
                });
            }
        }

        Ok(())
    }

    /// Read from input using the consumption strategy defined by the frame type
    ///
    /// The consumption strategy is automatically determined by `T::consumption_strategy()`:
    /// - **Video frames** (Latest): Discards old frames, returns newest available frame
    /// - **Audio frames** (Sequential): Returns frames in order, no skipping
    ///
    /// This is the primary read method - you don't need to choose a strategy manually.
    ///
    /// Returns `None` if:
    /// - No data available in the buffer
    /// - Port is disconnected (plug always returns None)
    pub fn read(&self) -> Option<T> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let connections = self.connections_mut();
            if let Some(conn) = connections.first_mut() {
                conn.read()
            } else {
                // Should never happen (we always have plug), but handle gracefully
                None
            }
        }
    }

    /// Peek at next item without consuming it
    pub fn peek(&self) -> Option<T> {
        let connections = self.connections();
        if let Some(conn) = connections.first() {
            match conn {
                InputConnection::Connected { consumer, .. } => consumer.peek(),
                InputConnection::Disconnected { .. } => None,
            }
        } else {
            None
        }
    }

    /// Check if port has any real connections (not just plugs)
    pub fn is_connected(&self) -> bool {
        self.connections().iter().any(|c| c.is_connected())
    }

    /// Get count of real connections (excluding plugs)
    pub fn connection_count(&self) -> usize {
        self.connections()
            .iter()
            .filter(|c| c.is_connected())
            .count()
    }
}
