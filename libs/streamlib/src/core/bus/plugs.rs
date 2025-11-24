//! Null object pattern for disconnected ports
//!
//! Every port always has at least one connection - either real or a "plug".
//! Plugs silently drop pushed data and always return None when popped.

use crate::core::bus::PortMessage;
use std::marker::PhantomData;

/// A producer that silently drops all pushed data (disconnected port)
pub struct DisconnectedProducer<T: PortMessage> {
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> Default for DisconnectedProducer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: PortMessage> DisconnectedProducer<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    /// Push succeeds but data is silently dropped
    pub fn push(&mut self, _value: T) -> Result<(), rtrb::PushError<T>> {
        // Intentionally drop the value
        Ok(())
    }
}

/// A consumer that always returns None (disconnected port)
pub struct DisconnectedConsumer<T: PortMessage> {
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> Default for DisconnectedConsumer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: PortMessage> DisconnectedConsumer<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    /// Pop always returns None (no data available)
    pub fn pop(&mut self) -> Result<Option<T>, rtrb::PopError> {
        Ok(None) // Always empty
    }
}
