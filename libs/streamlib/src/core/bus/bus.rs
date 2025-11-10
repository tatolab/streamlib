
use super::connection_manager::ConnectionManager;
use super::connection::ProcessorConnection;
use super::ports::{PortAddress, PortMessage};
use crate::core::Result;
use std::sync::Arc;
use parking_lot::RwLock;

pub struct Bus {
    manager: Arc<RwLock<ConnectionManager>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(ConnectionManager::new())),
        }
    }

    /// Create a generic connection between source and destination ports
    pub fn create_connection<T: PortMessage + 'static>(
        &self,
        source: PortAddress,
        dest: PortAddress,
        capacity: usize,
    ) -> Result<Arc<ProcessorConnection<T>>> {
        self.manager.write().create_connection(source, dest, capacity)
    }

    /// Get all connections from a source port
    pub fn connections_from_source<T: PortMessage + 'static>(
        &self,
        source: &PortAddress,
    ) -> Vec<Arc<ProcessorConnection<T>>> {
        self.manager.read().connections_from_source(source)
    }

    /// Get connection at destination port
    pub fn connection_at_dest<T: PortMessage + 'static>(
        &self,
        dest: &PortAddress,
    ) -> Option<Arc<ProcessorConnection<T>>> {
        self.manager.read().connection_at_dest(dest)
    }

    /// Disconnect a connection by ID
    pub fn disconnect(&self, id: super::connection::ConnectionId) -> Result<()> {
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
