//! Data bus implementation with message queue
//!
//! DataFrame messages are small metadata/control messages:
//! - Unbounded queue (MPMC channel)
//! - Each reader gets independent copy of messages
//! - No dropping - all messages preserved
//! - Thread-safe with crossbeam channels
//!
//! # Design
//!
//! - Uses crossbeam-channel for efficient MPMC
//! - Each reader has own receiver
//! - Writer broadcasts to all readers
//! - Messages cloned for each reader (small overhead for metadata)

use super::{Bus, BusId, BusReader};
use crate::core::DataFrame;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Data bus with broadcast queue
pub struct DataBus {
    id: BusId,
    /// List of all active reader senders
    /// When we write, we send to all of these
    readers: Arc<Mutex<Vec<Sender<DataFrame>>>>,
}

impl DataBus {
    pub fn new() -> Self {
        Self {
            id: BusId::new(),
            readers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Default for DataBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus<DataFrame> for DataBus {
    fn id(&self) -> BusId {
        self.id
    }

    fn create_reader(&self) -> Box<dyn BusReader<DataFrame>> {
        // Create new channel for this reader
        let (tx, rx) = unbounded();

        // Register sender with bus
        let mut readers = self.readers.lock().unwrap();
        readers.push(tx);

        tracing::debug!("[DataBus {}] Created reader (total: {})", self.id, readers.len());

        Box::new(DataBusReader {
            bus_id: self.id,
            rx,
        })
    }

    fn write(&self, message: DataFrame) {
        let readers = self.readers.lock().unwrap();

        // Broadcast to all readers
        for (i, sender) in readers.iter().enumerate() {
            if let Err(e) = sender.try_send(message.clone()) {
                tracing::warn!("[DataBus {}] Failed to send to reader {}: {}",
                    self.id, i, e);
            }
        }

        tracing::trace!("[DataBus {}] Broadcast message to {} readers",
            self.id, readers.len());
    }
}

/// Reader for DataBus
pub struct DataBusReader {
    bus_id: BusId,
    rx: Receiver<DataFrame>,
}

impl BusReader<DataFrame> for DataBusReader {
    fn read_latest(&mut self) -> Option<DataFrame> {
        // Try to get the most recent message
        // Drain all older messages and return the last one
        let mut latest = None;
        while let Ok(msg) = self.rx.try_recv() {
            latest = Some(msg);
        }

        if latest.is_some() {
            tracing::trace!("[DataBus {}] Reader read message", self.bus_id);
        }

        latest
    }

    fn has_data(&self) -> bool {
        !self.rx.is_empty()
    }

    fn clone_reader(&self) -> Box<dyn BusReader<DataFrame>> {
        // Cannot clone receiver - would need bus reference to create new one
        // This is a limitation of the current design
        // For now, panic - we'll need to refactor if reader cloning is needed
        panic!("DataBusReader cannot be cloned - needs bus reference to create new channel");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MetadataValue;
    use std::collections::HashMap;

    fn create_test_message(key: &str, value: i64) -> DataFrame {
        let mut metadata = HashMap::new();
        metadata.insert(key.to_string(), MetadataValue::Int(value));
        DataFrame::new(metadata, 0)
    }

    #[test]
    fn test_data_bus_basic() {
        let bus = DataBus::new();
        let mut reader = bus.create_reader();

        // No data initially
        assert!(!reader.has_data());
        assert!(reader.read_latest().is_none());

        // Write a message
        bus.write(create_test_message("test", 42));

        // Reader should have data
        assert!(reader.has_data());
        let msg = reader.read_latest();
        assert!(msg.is_some());
    }

    #[test]
    fn test_data_bus_fan_out() {
        let bus = DataBus::new();
        let mut reader1 = bus.create_reader();
        let mut reader2 = bus.create_reader();

        bus.write(create_test_message("test", 42));

        // Both readers get the message
        assert!(reader1.read_latest().is_some());
        assert!(reader2.read_latest().is_some());
    }

    #[test]
    fn test_data_bus_multiple_messages() {
        let bus = DataBus::new();
        let mut reader = bus.create_reader();

        // Write multiple messages
        bus.write(create_test_message("msg1", 1));
        bus.write(create_test_message("msg2", 2));
        bus.write(create_test_message("msg3", 3));

        // read_latest() drains and returns last
        let msg = reader.read_latest();
        assert!(msg.is_some());

        // No more messages
        assert!(reader.read_latest().is_none());
    }
}
