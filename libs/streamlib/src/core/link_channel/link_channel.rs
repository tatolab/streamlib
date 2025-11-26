//! LinkChannel - manages runtime link channels between processors

use super::link_channel_manager::LinkChannelManager;
use super::link_id::LinkId;
use super::link_owned_channel::{LinkOwnedConsumer, LinkOwnedProducer};
use super::link_ports::{LinkPortAddress, LinkPortMessage};
use crate::core::Result;
use parking_lot::RwLock;
use std::sync::Arc;

/// Manages link channels between processors
///
/// LinkChannel is the runtime component that creates and manages
/// the actual data flow channels between processor link ports.
pub struct LinkChannel {
    manager: Arc<RwLock<LinkChannelManager>>,
}

impl LinkChannel {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(LinkChannelManager::new())),
        }
    }

    /// Create a lock-free channel between source and destination link ports
    /// Returns owned producer/consumer pair for lock-free operations
    pub fn create_channel<T: LinkPortMessage + 'static>(
        &self,
        source: LinkPortAddress,
        dest: LinkPortAddress,
        capacity: usize,
    ) -> Result<(LinkOwnedProducer<T>, LinkOwnedConsumer<T>)> {
        self.manager.write().create_channel(source, dest, capacity)
    }

    /// Disconnect a link by ID
    ///
    /// Returns the source and destination LinkPortAddress if found, None if not found.
    pub fn disconnect(&self, id: LinkId) -> Option<(LinkPortAddress, LinkPortAddress)> {
        self.manager.write().disconnect(id)
    }

    /// Get total link count
    pub fn link_count(&self) -> usize {
        self.manager.read().link_count()
    }

    /// Check if destination is already linked
    pub fn is_dest_linked(&self, dest: &LinkPortAddress) -> bool {
        self.manager.read().is_dest_linked(dest)
    }
}

impl Default for LinkChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_channel_creation() {
        let lc = LinkChannel::new();
        assert_eq!(lc.link_count(), 0);
    }

    #[test]
    fn test_link_channel_default() {
        let lc = LinkChannel::default();
        assert_eq!(lc.link_count(), 0);
    }
}
