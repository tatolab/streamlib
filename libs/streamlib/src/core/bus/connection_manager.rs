use super::connection::{create_owned_connection, ConnectionId, OwnedConsumer, OwnedProducer};
use super::ports::{PortAddress, PortMessage};
use crate::core::{Result, StreamError};
use std::any::TypeId;
use std::collections::HashMap;

/// Metadata-only connection storage
/// Stores connection metadata without the actual connection (which is owned by processors)
// TODO(@jonathan): ConnectionMetadata fields (id, source, dest, type_id, type_name, capacity) are never read
// Review if this metadata tracking is still needed or can be removed
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ConnectionMetadata {
    id: ConnectionId,
    source: PortAddress,
    dest: PortAddress,
    type_id: TypeId,
    type_name: &'static str,
    capacity: usize,
}

/// Lock-free connection manager with metadata-only storage
/// Processors own their connections directly (OwnedProducer/OwnedConsumer)
pub struct ConnectionManager {
    // Metadata-only storage for owned connections
    metadata: HashMap<ConnectionId, ConnectionMetadata>,

    // Index: source port → list of connection IDs
    source_index: HashMap<PortAddress, Vec<ConnectionId>>,

    // Index: dest port → connection ID (enforces 1-to-1 at destination)
    dest_index: HashMap<PortAddress, ConnectionId>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            source_index: HashMap::new(),
            dest_index: HashMap::new(),
        }
    }

    /// Create a new lock-free connection between source and destination ports
    /// Returns owned producer/consumer pair that processors manage directly
    /// Enforces 1-to-1 rule: destination can only have one connection
    pub fn create_connection<T: PortMessage + 'static>(
        &mut self,
        source: PortAddress,
        dest: PortAddress,
        capacity: usize,
    ) -> Result<(OwnedProducer<T>, OwnedConsumer<T>)> {
        // Enforce 1-to-1: Check if dest already connected
        if self.dest_index.contains_key(&dest) {
            return Err(StreamError::Connection(format!(
                "Destination port {} already has a connection (1-to-1 rule)",
                dest.full_address()
            )));
        }

        // Create lock-free owned connection
        let (producer, consumer) = create_owned_connection::<T>(capacity);

        // Generate connection ID
        let conn_id = ConnectionId::new();
        let type_id = TypeId::of::<T>();

        // Store metadata only (not the actual connection)
        let metadata = ConnectionMetadata {
            id: conn_id,
            source: source.clone(),
            dest: dest.clone(),
            type_id,
            type_name: std::any::type_name::<T>(),
            capacity,
        };

        self.metadata.insert(conn_id, metadata);

        // Update source index (one source can have multiple connections)
        self.source_index
            .entry(source)
            .or_insert_with(Vec::new)
            .push(conn_id);

        // Update dest index (enforces 1-to-1)
        self.dest_index.insert(dest, conn_id);

        Ok((producer, consumer))
    }

    /// Disconnect and remove a connection
    pub fn disconnect(&mut self, id: ConnectionId) -> Result<()> {
        // Check if connection exists in metadata
        if self.metadata.remove(&id).is_some() {
            // Clean up indices
            // Remove from source index
            for (_, ids) in self.source_index.iter_mut() {
                ids.retain(|&cid| cid != id);
            }

            // Remove from dest index
            self.dest_index.retain(|_, &mut cid| cid != id);

            Ok(())
        } else {
            Err(StreamError::Connection(format!(
                "Connection {} not found",
                id.0
            )))
        }
    }

    /// Get total connection count
    pub fn connection_count(&self) -> usize {
        self.metadata.len()
    }

    /// Check if a destination port is already connected
    pub fn is_dest_connected(&self, dest: &PortAddress) -> bool {
        self.dest_index.contains_key(dest)
    }

    /// Get all connection IDs
    pub fn all_connections(&self) -> Vec<ConnectionId> {
        self.metadata.keys().copied().collect()
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_connection() {
        use crate::core::frames::DataFrame;

        let mut manager = ConnectionManager::new();
        let source = PortAddress::new("proc1", "out");
        let dest = PortAddress::new("proc2", "in");

        let result = manager.create_connection::<DataFrame>(source.clone(), dest.clone(), 4);
        assert!(result.is_ok());

        // Verify connection was registered
        assert!(manager.is_dest_connected(&dest));
        assert_eq!(manager.connection_count(), 1);
    }

    #[test]
    fn test_one_to_one_enforcement() {
        use crate::core::frames::DataFrame;

        let mut manager = ConnectionManager::new();
        let source1 = PortAddress::new("proc1", "out");
        let source2 = PortAddress::new("proc2", "out");
        let dest = PortAddress::new("proc3", "in");

        // First connection should succeed
        let result1 = manager.create_connection::<DataFrame>(source1, dest.clone(), 4);
        assert!(result1.is_ok());

        // Verify destination is connected
        assert!(manager.is_dest_connected(&dest));

        // Second connection to same destination should fail (1-to-1 rule)
        let result2 = manager.create_connection::<DataFrame>(source2, dest, 4);
        assert!(result2.is_err());

        if let Err(e) = result2 {
            assert!(matches!(e, StreamError::Connection(_)));
        }
    }

    #[test]
    fn test_multiple_outputs_allowed() {
        use crate::core::frames::DataFrame;

        let mut manager = ConnectionManager::new();
        let source = PortAddress::new("proc1", "out");
        let dest1 = PortAddress::new("proc2", "in");
        let dest2 = PortAddress::new("proc3", "in");

        // Source can connect to multiple destinations
        let result1 = manager.create_connection::<DataFrame>(source.clone(), dest1, 4);
        assert!(result1.is_ok());

        let result2 = manager.create_connection::<DataFrame>(source, dest2, 4);
        assert!(result2.is_ok());
    }

    #[test]
    fn test_connection_count() {
        use crate::core::frames::DataFrame;

        let mut manager = ConnectionManager::new();
        assert_eq!(manager.connection_count(), 0);

        let source = PortAddress::new("proc1", "out");
        let dest = PortAddress::new("proc2", "in");

        manager
            .create_connection::<DataFrame>(source, dest, 4)
            .unwrap();
        assert_eq!(manager.connection_count(), 1);
    }
}
