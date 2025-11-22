use super::connection::{OwnedConsumer, OwnedProducer};
use super::connection_manager::ConnectionManager;
use super::ports::{PortAddress, PortMessage};
use crate::core::Result;
use parking_lot::RwLock;
use std::sync::Arc;

pub struct Bus {
    manager: Arc<RwLock<ConnectionManager>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(ConnectionManager::new())),
        }
    }

    /// Create a lock-free connection between source and destination ports
    /// Returns owned producer/consumer pair for lock-free operations
    pub fn create_connection<T: PortMessage + 'static>(
        &self,
        source: PortAddress,
        dest: PortAddress,
        capacity: usize,
    ) -> Result<(OwnedProducer<T>, OwnedConsumer<T>)> {
        self.manager
            .write()
            .create_connection(source, dest, capacity)
    }

    /// Disconnect a connection by ID
    ///
    /// Returns the source and destination PortAddress if found, None if not found.
    pub fn disconnect(
        &self,
        id: super::connection::ConnectionId,
    ) -> Option<(PortAddress, PortAddress)> {
        self.manager.write().disconnect(id)
    }

    /// Get total connection count
    pub fn connection_count(&self) -> usize {
        self.manager.read().connection_count()
    }

    /// Check if destination is already connected
    pub fn is_dest_connected(&self, dest: &PortAddress) -> bool {
        self.manager.read().is_dest_connected(dest)
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bus_creation() {
        let bus = Bus::new();
        assert_eq!(bus.connection_count(), 0);
    }

    #[test]
    fn test_bus_default() {
        let bus = Bus::default();
        assert_eq!(bus.connection_count(), 0);
    }
}
