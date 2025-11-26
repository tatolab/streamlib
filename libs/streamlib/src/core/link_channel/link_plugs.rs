use super::link_ports::LinkPortMessage;
use std::marker::PhantomData;

/// A producer that silently drops all pushed data (disconnected link port)
pub struct LinkDisconnectedProducer<T: LinkPortMessage> {
    _phantom: PhantomData<T>,
}

impl<T: LinkPortMessage> Default for LinkDisconnectedProducer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: LinkPortMessage> LinkDisconnectedProducer<T> {
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

/// A consumer that always returns None (disconnected link port)
pub struct LinkDisconnectedConsumer<T: LinkPortMessage> {
    _phantom: PhantomData<T>,
}

impl<T: LinkPortMessage> Default for LinkDisconnectedConsumer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: LinkPortMessage> LinkDisconnectedConsumer<T> {
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
