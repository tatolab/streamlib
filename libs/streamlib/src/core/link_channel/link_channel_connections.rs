//! Link channel connection types supporting both real and plugged connections

use super::link_plugs::{LinkDisconnectedConsumer, LinkDisconnectedProducer};
use super::link_ports::ConsumptionStrategy;
use super::{
    LinkId, LinkOwnedConsumer, LinkOwnedProducer, LinkPortAddress, LinkPortMessage, LinkWakeupEvent,
};
use crossbeam_channel::Sender;

/// Output link connection - either linked to another processor or disconnected (plug)
pub enum LinkOutputConnection<T: LinkPortMessage> {
    /// Linked to another processor
    Connected {
        id: LinkId,
        producer: LinkOwnedProducer<T>,
        wakeup: Sender<LinkWakeupEvent>,
    },

    /// Disconnected plug (silently drops data)
    Disconnected {
        id: LinkId,
        plug: LinkDisconnectedProducer<T>,
    },
}

impl<T: LinkPortMessage> LinkOutputConnection<T> {
    /// Push data to link (works for both Connected and Disconnected)
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
            let _ = wakeup.send(LinkWakeupEvent::DataAvailable);
        }
        // Disconnected has no wakeup - no-op
    }

    /// Get link ID
    pub fn id(&self) -> &LinkId {
        match self {
            Self::Connected { id, .. } => id,
            Self::Disconnected { id, .. } => id,
        }
    }

    /// Check if this is a real link (not a plug)
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Input link connection - either linked to another processor or disconnected (plug)
pub enum LinkInputConnection<T: LinkPortMessage> {
    /// Linked to another processor
    Connected {
        id: LinkId,
        consumer: LinkOwnedConsumer<T>,
        source_address: LinkPortAddress,
        wakeup: Sender<LinkWakeupEvent>,
    },

    /// Disconnected plug (always returns None)
    Disconnected {
        id: LinkId,
        plug: LinkDisconnectedConsumer<T>,
    },
}

impl<T: LinkPortMessage> LinkInputConnection<T> {
    /// Read using sequential strategy (in-order consumption, required for audio)
    fn read_sequential(&mut self) -> Option<T> {
        match self {
            Self::Connected { consumer, .. } => consumer.read(),
            Self::Disconnected { plug, .. } => plug.pop().ok().flatten(),
        }
    }

    /// Read using latest strategy (discard old frames, optimal for video)
    fn read_latest(&mut self) -> Option<T> {
        match self {
            Self::Connected { consumer, .. } => consumer.read_latest(),
            Self::Disconnected { plug, .. } => plug.pop().ok().flatten(),
        }
    }

    /// Read from link using the consumption strategy defined by the frame type
    ///
    /// - Video frames: Uses Latest strategy (discards old frames to show newest)
    /// - Audio frames: Uses Sequential strategy (consumes all frames in order)
    ///
    /// This is the primary read method - the strategy is determined automatically
    /// based on `T::consumption_strategy()`.
    pub fn read(&mut self) -> Option<T> {
        match T::consumption_strategy() {
            ConsumptionStrategy::Latest => self.read_latest(),
            ConsumptionStrategy::Sequential => self.read_sequential(),
        }
    }

    /// Get link ID
    pub fn id(&self) -> &LinkId {
        match self {
            Self::Connected { id, .. } => id,
            Self::Disconnected { id, .. } => id,
        }
    }

    /// Check if this is a real link (not a plug)
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}
