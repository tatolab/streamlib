//! LinkChannelManager - manages metadata for link channels

use super::link_id::{self, LinkId};
use super::link_owned_channel::{create_link_channel, LinkOwnedConsumer, LinkOwnedProducer};
use super::link_ports::{LinkPortAddress, LinkPortMessage};
use crate::core::{Result, StreamError};
use std::any::TypeId;
use std::collections::HashMap;

/// Metadata-only link storage
/// Stores link metadata without the actual channel (which is owned by processors)
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct LinkMetadata {
    id: LinkId,
    source: LinkPortAddress,
    dest: LinkPortAddress,
    type_id: TypeId,
    type_name: &'static str,
    capacity: usize,
}

/// Lock-free link channel manager with metadata-only storage
/// Processors own their channels directly (LinkOwnedProducer/LinkOwnedConsumer)
pub struct LinkChannelManager {
    /// Metadata-only storage for owned channels
    metadata: HashMap<LinkId, LinkMetadata>,

    /// Index: source link port → list of link IDs
    source_index: HashMap<LinkPortAddress, Vec<LinkId>>,

    /// Index: dest link port → link ID (enforces 1-to-1 at destination)
    dest_index: HashMap<LinkPortAddress, LinkId>,
}

impl LinkChannelManager {
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            source_index: HashMap::new(),
            dest_index: HashMap::new(),
        }
    }

    /// Create a new lock-free channel between source and destination link ports
    /// Returns owned producer/consumer pair that processors manage directly
    /// Enforces 1-to-1 rule: destination can only have one link
    pub fn create_channel<T: LinkPortMessage + 'static>(
        &mut self,
        source: LinkPortAddress,
        dest: LinkPortAddress,
        capacity: usize,
    ) -> Result<(LinkOwnedProducer<T>, LinkOwnedConsumer<T>)> {
        // Enforce 1-to-1: Check if dest already linked
        if self.dest_index.contains_key(&dest) {
            return Err(StreamError::Link(format!(
                "Destination link port {} already has a link (1-to-1 rule)",
                dest.full_address()
            )));
        }

        // Create lock-free owned channel
        let (producer, consumer) = create_link_channel::<T>(capacity);

        // Generate link ID (format: "source->dest")
        let link_id = link_id::__private::new_unchecked(format!(
            "{}->{}",
            source.full_address(),
            dest.full_address()
        ));
        let type_id = TypeId::of::<T>();

        // Store metadata only (not the actual channel)
        let link_metadata = LinkMetadata {
            id: link_id.clone(),
            source: source.clone(),
            dest: dest.clone(),
            type_id,
            type_name: std::any::type_name::<T>(),
            capacity,
        };

        self.metadata.insert(link_id.clone(), link_metadata);

        // Update source index (one source can have multiple links)
        self.source_index
            .entry(source)
            .or_default()
            .push(link_id.clone());

        // Update dest index (enforces 1-to-1)
        self.dest_index.insert(dest, link_id);

        Ok((producer, consumer))
    }

    /// Disconnect and remove a link
    ///
    /// Returns the source and destination LinkPortAddress if found, so the runtime
    /// can clean up processor link ports. Returns None if link doesn't exist.
    pub fn disconnect(&mut self, id: LinkId) -> Option<(LinkPortAddress, LinkPortAddress)> {
        if let Some(link_metadata) = self.metadata.remove(&id) {
            let source = link_metadata.source.clone();
            let dest = link_metadata.dest.clone();

            // Clean up indices
            // Remove from source index
            for (_, ids) in self.source_index.iter_mut() {
                ids.retain(|lid| lid != &id);
            }

            // Remove from dest index
            self.dest_index.retain(|_, lid| lid != &id);

            Some((source, dest))
        } else {
            None
        }
    }

    /// Get total link count
    pub fn link_count(&self) -> usize {
        self.metadata.len()
    }

    /// Check if a destination link port is already linked
    pub fn is_dest_linked(&self, dest: &LinkPortAddress) -> bool {
        self.dest_index.contains_key(dest)
    }

    /// Get all link IDs
    pub fn all_links(&self) -> Vec<LinkId> {
        self.metadata.keys().cloned().collect()
    }
}

impl Default for LinkChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_channel() {
        use crate::core::frames::DataFrame;

        let mut manager = LinkChannelManager::new();
        let source = LinkPortAddress::new("proc1", "out");
        let dest = LinkPortAddress::new("proc2", "in");

        let result = manager.create_channel::<DataFrame>(source.clone(), dest.clone(), 4);
        assert!(result.is_ok());

        // Verify link was registered
        assert!(manager.is_dest_linked(&dest));
        assert_eq!(manager.link_count(), 1);
    }

    #[test]
    fn test_one_to_one_enforcement() {
        use crate::core::frames::DataFrame;

        let mut manager = LinkChannelManager::new();
        let source1 = LinkPortAddress::new("proc1", "out");
        let source2 = LinkPortAddress::new("proc2", "out");
        let dest = LinkPortAddress::new("proc3", "in");

        // First link should succeed
        let result1 = manager.create_channel::<DataFrame>(source1, dest.clone(), 4);
        assert!(result1.is_ok());

        // Verify destination is linked
        assert!(manager.is_dest_linked(&dest));

        // Second link to same destination should fail (1-to-1 rule)
        let result2 = manager.create_channel::<DataFrame>(source2, dest, 4);
        assert!(result2.is_err());

        if let Err(e) = result2 {
            assert!(matches!(e, StreamError::Link(_)));
        }
    }

    #[test]
    fn test_multiple_outputs_allowed() {
        use crate::core::frames::DataFrame;

        let mut manager = LinkChannelManager::new();
        let source = LinkPortAddress::new("proc1", "out");
        let dest1 = LinkPortAddress::new("proc2", "in");
        let dest2 = LinkPortAddress::new("proc3", "in");

        // Source can link to multiple destinations
        let result1 = manager.create_channel::<DataFrame>(source.clone(), dest1, 4);
        assert!(result1.is_ok());

        let result2 = manager.create_channel::<DataFrame>(source, dest2, 4);
        assert!(result2.is_ok());
    }

    #[test]
    fn test_link_count() {
        use crate::core::frames::DataFrame;

        let mut manager = LinkChannelManager::new();
        assert_eq!(manager.link_count(), 0);

        let source = LinkPortAddress::new("proc1", "out");
        let dest = LinkPortAddress::new("proc2", "in");

        manager
            .create_channel::<DataFrame>(source, dest, 4)
            .unwrap();
        assert_eq!(manager.link_count(), 1);
    }
}
