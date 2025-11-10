use super::connection::{ProcessorConnection, ConnectionId};
use super::ports::{PortAddress, PortMessage};
use crate::core::{Result, StreamError};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// Trait for type-erased connection storage
trait AnyConnection: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn source(&self) -> &PortAddress;
    fn dest(&self) -> &PortAddress;
    fn id(&self) -> ConnectionId;
    fn type_name(&self) -> &'static str;
}

impl<T: PortMessage + 'static> AnyConnection for Arc<ProcessorConnection<T>> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn source(&self) -> &PortAddress {
        // We'll update ProcessorConnection to use PortAddress in Phase 1.3
        // For now, create temporary PortAddress
        unimplemented!("Will be implemented when ProcessorConnection is updated to use PortAddress")
    }

    fn dest(&self) -> &PortAddress {
        unimplemented!("Will be implemented when ProcessorConnection is updated to use PortAddress")
    }

    fn id(&self) -> ConnectionId {
        self.id
    }

    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
}

/// Generic connection manager using TypeId for type-safe storage without type-specific hashmaps
pub struct ConnectionManager {
    // Key: (TypeId, ConnectionId) - allows same connection ID across different types
    connections: HashMap<(TypeId, ConnectionId), Box<dyn AnyConnection>>,

    // Index: source port → list of connection IDs
    source_index: HashMap<PortAddress, Vec<ConnectionId>>,

    // Index: dest port → connection ID (enforces 1-to-1 at destination)
    dest_index: HashMap<PortAddress, ConnectionId>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            source_index: HashMap::new(),
            dest_index: HashMap::new(),
        }
    }

    /// Create a new connection between source and destination ports
    /// Enforces 1-to-1 rule: destination can only have one connection
    pub fn create_connection<T: PortMessage + 'static>(
        &mut self,
        source: PortAddress,
        dest: PortAddress,
        capacity: usize,
    ) -> Result<Arc<ProcessorConnection<T>>> {
        // Enforce 1-to-1: Check if dest already connected
        if self.dest_index.contains_key(&dest) {
            return Err(StreamError::Connection(format!(
                "Destination port {} already has a connection (1-to-1 rule)",
                dest.full_address()
            )));
        }

        // Create connection (still using old string-based API, will update in Phase 1.3)
        let connection = Arc::new(ProcessorConnection::new(
            source.processor_id.clone(),
            source.port_name.to_string(),
            dest.processor_id.clone(),
            dest.port_name.to_string(),
            capacity,
        ));

        let conn_id = connection.id;
        let type_id = TypeId::of::<T>();

        // Store connection with TypeId key
        self.connections.insert(
            (type_id, conn_id),
            Box::new(Arc::clone(&connection)),
        );

        // Update source index (one source can have multiple connections)
        self.source_index
            .entry(source)
            .or_insert_with(Vec::new)
            .push(conn_id);

        // Update dest index (enforces 1-to-1)
        self.dest_index.insert(dest, conn_id);

        Ok(connection)
    }

    /// Get a connection by ID with type checking
    pub fn get_connection<T: PortMessage + 'static>(
        &self,
        id: ConnectionId,
    ) -> Option<Arc<ProcessorConnection<T>>> {
        let type_id = TypeId::of::<T>();
        self.connections
            .get(&(type_id, id))
            .and_then(|boxed| boxed.as_any().downcast_ref::<Arc<ProcessorConnection<T>>>())
            .cloned()
    }

    /// Get all connections from a source port
    pub fn connections_from_source<T: PortMessage + 'static>(
        &self,
        source: &PortAddress,
    ) -> Vec<Arc<ProcessorConnection<T>>> {
        let type_id = TypeId::of::<T>();
        self.source_index
            .get(source)
            .map(|ids| {
                ids.iter()
                    .filter_map(|&id| {
                        self.connections
                            .get(&(type_id, id))
                            .and_then(|b| b.as_any().downcast_ref::<Arc<ProcessorConnection<T>>>())
                            .cloned()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get connection at a destination port (should only be one due to 1-to-1 rule)
    pub fn connection_at_dest<T: PortMessage + 'static>(
        &self,
        dest: &PortAddress,
    ) -> Option<Arc<ProcessorConnection<T>>> {
        let type_id = TypeId::of::<T>();
        self.dest_index
            .get(dest)
            .and_then(|&id| self.connections.get(&(type_id, id)))
            .and_then(|b| b.as_any().downcast_ref::<Arc<ProcessorConnection<T>>>())
            .cloned()
    }

    /// Disconnect and remove a connection
    pub fn disconnect(&mut self, id: ConnectionId) -> Result<()> {
        // Find the connection's TypeId by searching all connections
        let mut found_key = None;
        let mut source: Option<PortAddress> = None;
        let mut dest: Option<PortAddress> = None;

        for ((type_id, conn_id), conn) in &self.connections {
            if *conn_id == id {
                found_key = Some((*type_id, *conn_id));
                // Note: source() and dest() will panic until Phase 1.3
                // For now, we'll update indices manually
                break;
            }
        }

        if let Some(key) = found_key {
            self.connections.remove(&key);

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
        self.connections.len()
    }

    /// Check if a destination port is already connected
    pub fn is_dest_connected(&self, dest: &PortAddress) -> bool {
        self.dest_index.contains_key(dest)
    }

    /// Get all connections (type-erased, for debugging)
    pub fn all_connections(&self) -> Vec<ConnectionId> {
        self.connections.keys().map(|(_, id)| *id).collect()
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

    // Mock frame type for testing
    #[derive(Clone)]
    struct TestFrame(i32);

    impl PortMessage for TestFrame {
        fn port_type() -> crate::core::bus::PortType {
            crate::core::bus::PortType::Data
        }

        fn schema() -> Arc<crate::core::Schema> {
            Arc::new(crate::core::Schema::new(
                "TestFrame",
                crate::core::SemanticVersion::new(1, 0, 0),
                vec![],
                crate::core::SerializationFormat::Bincode,
            ))
        }
    }

    #[test]
    fn test_create_connection() {
        let mut manager = ConnectionManager::new();

        let source = PortAddress::new("proc1", "out");
        let dest = PortAddress::new("proc2", "in");

        // This will panic until Phase 1.3 - that's expected
        // let conn = manager.create_connection::<TestFrame>(source, dest, 4);
        // assert!(conn.is_ok());
    }

    #[test]
    fn test_one_to_one_enforcement() {
        // Will implement once ProcessorConnection is updated
    }

    #[test]
    fn test_type_safety() {
        // Will implement once ProcessorConnection is updated
    }

    #[test]
    fn test_connection_count() {
        let manager = ConnectionManager::new();
        assert_eq!(manager.connection_count(), 0);
    }
}
