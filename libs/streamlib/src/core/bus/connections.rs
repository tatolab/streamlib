//! Unified connection types supporting both real and plugged connections

use crate::core::bus::plugs::{DisconnectedConsumer, DisconnectedProducer};
use crate::core::bus::{OwnedConsumer, OwnedProducer, PortAddress, PortMessage};
use crate::core::runtime::{ConnectionId, WakeupEvent};
use crossbeam_channel::Sender;

/// Output connection - either connected to another processor or disconnected (plug)
pub enum OutputConnection<T: PortMessage> {
    /// Connected to another processor
    Connected {
        id: ConnectionId,
        producer: OwnedProducer<T>,
        wakeup: Sender<WakeupEvent>,
    },

    /// Disconnected plug (silently drops data)
    Disconnected {
        id: ConnectionId,
        plug: DisconnectedProducer<T>,
    },
}

impl<T: PortMessage> OutputConnection<T> {
    /// Push to connection (works for both Connected and Disconnected)
    pub fn push(&mut self, value: T) -> Result<(), rtrb::PushError<T>> {
        match self {
            Self::Connected { producer, .. } => {
                producer.write(value);
                Ok(())
            }
            Self::Disconnected { plug, .. } => plug.push(value),
        }
    }

    /// Send wakeup to downstream processor (only for Connected)
    pub fn wake(&self) {
        if let Self::Connected { wakeup, .. } = self {
            // Ignore send errors (downstream processor may have stopped)
            let _ = wakeup.send(WakeupEvent::DataAvailable);
        }
        // Disconnected has no wakeup - no-op
    }

    /// Get connection ID
    pub fn id(&self) -> &ConnectionId {
        match self {
            Self::Connected { id, .. } => id,
            Self::Disconnected { id, .. } => id,
        }
    }

    /// Check if this is a real connection (not a plug)
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Input connection - either connected to another processor or disconnected (plug)
pub enum InputConnection<T: PortMessage> {
    /// Connected to another processor
    Connected {
        id: ConnectionId,
        consumer: OwnedConsumer<T>,
        source_address: PortAddress,
        wakeup: Sender<WakeupEvent>,
    },

    /// Disconnected plug (always returns None)
    Disconnected {
        id: ConnectionId,
        plug: DisconnectedConsumer<T>,
    },
}

impl<T: PortMessage> InputConnection<T> {
    /// Pop from connection (works for both Connected and Disconnected)
    pub fn pop(&mut self) -> Result<Option<T>, rtrb::PopError> {
        match self {
            Self::Connected { consumer, .. } => Ok(consumer.read()),
            Self::Disconnected { plug, .. } => plug.pop(),
        }
    }

    /// Get connection ID
    pub fn id(&self) -> &ConnectionId {
        match self {
            Self::Connected { id, .. } => id,
            Self::Disconnected { id, .. } => id,
        }
    }

    /// Check if this is a real connection (not a plug)
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}
