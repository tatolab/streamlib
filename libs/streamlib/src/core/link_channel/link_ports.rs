use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::sync::Arc;

use super::link_channel_connections::{LinkInputConnection, LinkOutputConnection};
use super::link_id::{self, LinkId};
use super::link_owned_channel::{LinkOwnedConsumer, LinkOwnedProducer};
use super::link_plugs::{LinkDisconnectedConsumer, LinkDisconnectedProducer};
use super::link_wakeup::LinkWakeupEvent;
use crate::core::graph::ProcessorId;
use crate::core::Result;
use crate::StreamError;
use crossbeam_channel::Sender;

/// Strongly-typed link port address combining processor ID and port name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkPortAddress {
    pub processor_id: ProcessorId,
    pub port_name: Cow<'static, str>,
}

impl LinkPortAddress {
    /// Create a new link port address
    pub fn new(processor: impl Into<ProcessorId>, port: impl Into<Cow<'static, str>>) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: port.into(),
        }
    }

    /// Create a link port address with a static string port name (zero allocation)
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

/// Type of data that flows through a link port
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPortType {
    Video,
    Audio,
    Data,
}

/// Sealed trait pattern - only known frame types can implement LinkPortMessage
pub mod sealed {
    pub trait Sealed {}
}

/// Consumption strategy for reading from link ports
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumptionStrategy {
    /// Read the latest item, discarding older ones (optimal for video)
    Latest,
    /// Read items sequentially in order (required for audio)
    Sequential,
}

/// Trait for types that can be sent through link ports
///
/// This is a sealed trait - only types in this crate can implement it.
pub trait LinkPortMessage: sealed::Sealed + Clone + Send + 'static {
    fn port_type() -> LinkPortType;
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

impl LinkPortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            LinkPortType::Video => 3,
            LinkPortType::Audio => 32,
            LinkPortType::Data => 16,
        }
    }

    pub fn compatible_with(&self, other: &LinkPortType) -> bool {
        self == other
    }
}

/// Inner implementation of LinkOutput
struct LinkOutputInner<T: LinkPortMessage> {
    /// Output link connections wrapped in UnsafeCell for interior mutability
    /// SAFETY: Only the owning processor thread ever accesses this
    output_link_connections: UnsafeCell<Vec<LinkOutputConnection<T>>>,
}

// SAFETY: LinkOutputInner can be safely shared between threads because:
// 1. Each processor thread has exclusive logical ownership of its outputs
// 2. Link connections are only modified during setup and disconnect operations
// 3. UnsafeCell is used for interior mutability with manual synchronization guarantee
// 4. LinkOwnedProducer and Sender<LinkWakeupEvent> operations are atomic
unsafe impl<T: LinkPortMessage> Sync for LinkOutputInner<T> {}
unsafe impl<T: LinkPortMessage> Send for LinkOutputInner<T> {}

/// Output link port for a processor
///
/// Always has at least one connection (real link or disconnected plug).
/// Internally uses Arc for sharing and UnsafeCell for interior mutability,
/// allowing ergonomic `&self` methods while maintaining lock-free performance.
pub struct LinkOutput<T: LinkPortMessage> {
    inner: Arc<LinkOutputInner<T>>,
}

impl<T: LinkPortMessage> Clone for LinkOutput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: LinkPortMessage> LinkOutput<T> {
    /// Create new output link port with default disconnected plug
    pub fn new(port_name: &str) -> Self {
        let plug_id = link_id::__private::new_unchecked(format!("{}.disconnected_plug", port_name));
        Self {
            inner: Arc::new(LinkOutputInner {
                output_link_connections: UnsafeCell::new(vec![
                    LinkOutputConnection::Disconnected {
                        id: plug_id,
                        plug: LinkDisconnectedProducer::new(),
                    },
                ]),
            }),
        }
    }

    /// SAFETY: Get mutable access to output link connections
    /// Safe because processor has exclusive ownership (single-threaded access)
    #[inline]
    #[allow(clippy::mut_from_ref)] // Intentional interior mutability pattern with UnsafeCell
    unsafe fn output_link_connections_mut(&self) -> &mut Vec<LinkOutputConnection<T>> {
        &mut *self.inner.output_link_connections.get()
    }

    /// Get immutable access to output link connections
    #[inline]
    fn output_link_connections(&self) -> &Vec<LinkOutputConnection<T>> {
        unsafe { &*self.inner.output_link_connections.get() }
    }

    /// Add a real link to this output port
    ///
    /// # Errors
    /// Returns error if a link with this ID already exists
    pub fn add_link(
        &self,
        link_id: LinkId,
        producer: LinkOwnedProducer<T>,
        wakeup: Sender<LinkWakeupEvent>,
    ) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let output_link_connections = self.output_link_connections_mut();

            // Check for duplicate
            if output_link_connections.iter().any(|c| c.id() == &link_id) {
                return Err(StreamError::LinkAlreadyExists(link_id.to_string()));
            }

            output_link_connections.push(LinkOutputConnection::Connected {
                id: link_id,
                producer,
                wakeup,
            });
        }

        Ok(())
    }

    /// Remove a link by ID
    ///
    /// # Errors
    /// Returns error if link was not found
    ///
    /// Automatically restores plug if last link removed
    pub fn remove_link(&self, link_id: &LinkId) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let output_link_connections = self.output_link_connections_mut();

            let idx = output_link_connections
                .iter()
                .position(|c: &LinkOutputConnection<T>| c.id() == link_id)
                .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;

            output_link_connections.swap_remove(idx);

            // If no links left, add plug back
            if output_link_connections.is_empty() {
                let plug_id = link_id::__private::new_unchecked(format!(
                    "{}.disconnected_plug_restored",
                    link_id
                ));
                output_link_connections.push(LinkOutputConnection::Disconnected {
                    id: plug_id,
                    plug: LinkDisconnectedProducer::new(),
                });
            }
        }

        Ok(())
    }

    /// Push data to all output links (including plugs)
    ///
    /// Clones data for all links except the last (which consumes via move)
    pub fn push(&self, value: T)
    where
        T: Clone,
    {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let output_link_connections = self.output_link_connections_mut();

            if output_link_connections.is_empty() {
                // Should never happen (we always have plug), but handle gracefully
                tracing::warn!("LinkOutput::push called with no links (impossible)");
                return;
            }

            let len = output_link_connections.len();

            // Clone for all except last
            for link_conn in &mut output_link_connections[..len - 1] {
                let _ = link_conn.push(value.clone());
                link_conn.wake();
            }

            // Move into last link connection
            if let Some(last) = output_link_connections.last_mut() {
                let _ = last.push(value);
                last.wake();
            }
        }
    }

    /// Alias for push() - convenience method
    pub fn write(&self, value: T)
    where
        T: Clone,
    {
        self.push(value)
    }

    /// Check if port has any real links (not just plugs)
    pub fn is_connected(&self) -> bool {
        self.output_link_connections()
            .iter()
            .any(|c: &LinkOutputConnection<T>| c.is_connected())
    }

    /// Get count of real links (excluding plugs)
    pub fn link_count(&self) -> usize {
        self.output_link_connections()
            .iter()
            .filter(|c: &&LinkOutputConnection<T>| c.is_connected())
            .count()
    }
}

/// Inner state for LinkInput
struct LinkInputInner<T: LinkPortMessage> {
    /// Input link connections (always contains at least one - plug if disconnected)
    input_link_connections: UnsafeCell<Vec<LinkInputConnection<T>>>,
}

// SAFETY: LinkInputInner can be safely shared between threads because:
// 1. Interior UnsafeCell is only accessed by single processor thread (runtime enforced)
// 2. Arc provides safe sharing across thread boundaries
// 3. LinkOwnedConsumer operations are atomic
unsafe impl<T: LinkPortMessage> Send for LinkInputInner<T> {}
unsafe impl<T: LinkPortMessage> Sync for LinkInputInner<T> {}

/// Input link port for a processor
///
/// Always has at least one connection (real link or disconnected plug).
/// Uses interior mutability pattern (UnsafeCell) to allow &self methods,
/// enabling Arc<LinkInput<T>> to work without additional Mutex wrapper.
///
/// SAFETY: Single-threaded processor access makes UnsafeCell safe:
/// - Only one processor thread accesses this port
/// - Runtime manages lifecycle (setup before processing, teardown after)
/// - LinkOwnedConsumer operations are already atomic
pub struct LinkInput<T: LinkPortMessage> {
    inner: Arc<LinkInputInner<T>>,
}

impl<T: LinkPortMessage> Clone for LinkInput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: LinkPortMessage> LinkInput<T> {
    /// Create new input link port with default disconnected plug
    pub fn new(port_name: &str) -> Self {
        let plug_id = link_id::__private::new_unchecked(format!("{}.disconnected_plug", port_name));
        Self {
            inner: Arc::new(LinkInputInner {
                input_link_connections: UnsafeCell::new(vec![LinkInputConnection::Disconnected {
                    id: plug_id,
                    plug: LinkDisconnectedConsumer::new(),
                }]),
            }),
        }
    }

    /// SAFETY: Get mutable access to input link connections
    /// Safe because processor has exclusive ownership (single-threaded access)
    #[inline]
    #[allow(clippy::mut_from_ref)] // Intentional interior mutability pattern with UnsafeCell
    unsafe fn input_link_connections_mut(&self) -> &mut Vec<LinkInputConnection<T>> {
        &mut *self.inner.input_link_connections.get()
    }

    /// Get immutable access to input link connections
    #[inline]
    fn input_link_connections(&self) -> &Vec<LinkInputConnection<T>> {
        unsafe { &*self.inner.input_link_connections.get() }
    }

    /// Add a real link to this input port
    ///
    /// # Errors
    /// Returns error if a link with this ID already exists
    ///
    /// Removes plug if this is the first real link
    pub fn add_link(
        &self,
        link_id: LinkId,
        consumer: LinkOwnedConsumer<T>,
        source_address: LinkPortAddress,
        wakeup: Sender<LinkWakeupEvent>,
    ) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let input_link_connections = self.input_link_connections_mut();

            // Check for duplicate
            if input_link_connections.iter().any(|c| c.id() == &link_id) {
                return Err(StreamError::LinkAlreadyExists(link_id.to_string()));
            }

            // Remove plug if this is the first real link
            if input_link_connections.len() == 1 && !input_link_connections[0].is_connected() {
                input_link_connections.clear();
            }

            input_link_connections.push(LinkInputConnection::Connected {
                id: link_id,
                consumer,
                source_address,
                wakeup,
            });
        }

        Ok(())
    }

    /// Remove a link by ID
    ///
    /// # Errors
    /// Returns error if link was not found
    ///
    /// Automatically restores plug if last link removed
    pub fn remove_link(&self, link_id: &LinkId) -> Result<()> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let input_link_connections = self.input_link_connections_mut();

            let idx = input_link_connections
                .iter()
                .position(|c: &LinkInputConnection<T>| c.id() == link_id)
                .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;

            input_link_connections.swap_remove(idx);

            // If no links left, add plug back
            if input_link_connections.is_empty() {
                let plug_id = link_id::__private::new_unchecked(format!(
                    "{}.disconnected_plug_restored",
                    link_id
                ));
                input_link_connections.push(LinkInputConnection::Disconnected {
                    id: plug_id,
                    plug: LinkDisconnectedConsumer::new(),
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
    /// Returns `None` if no data available or port is disconnected
    pub fn read(&self) -> Option<T> {
        // SAFETY: Processor has exclusive ownership
        unsafe {
            let input_link_connections = self.input_link_connections_mut();
            if let Some(link_conn) = input_link_connections.first_mut() {
                link_conn.read()
            } else {
                None
            }
        }
    }

    /// Peek at next item without consuming it
    pub fn peek(&self) -> Option<T> {
        let input_link_connections = self.input_link_connections();
        if let Some(link_conn) = input_link_connections.first() {
            match link_conn {
                LinkInputConnection::Connected { consumer, .. } => consumer.peek(),
                LinkInputConnection::Disconnected { .. } => None,
            }
        } else {
            None
        }
    }

    /// Check if port has any real links (not just plugs)
    pub fn is_connected(&self) -> bool {
        self.input_link_connections()
            .iter()
            .any(|c| c.is_connected())
    }

    /// Get count of real links (excluding plugs)
    pub fn link_count(&self) -> usize {
        self.input_link_connections()
            .iter()
            .filter(|c| c.is_connected())
            .count()
    }
}
