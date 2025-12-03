//! LinkInstanceManager - manages LinkInstance lifecycle and storage.

use super::graph::LinkId;
use super::runtime::{BoxedLinkInstance, LinkInputDataReader, LinkInstance, LinkOutputDataWriter};
use super::traits::{LinkPortAddress, LinkPortMessage};
use crate::core::{Result, StreamError};
use std::any::TypeId;
use std::collections::HashMap;

/// Metadata for a link.
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

/// Manages the lifecycle of link instances.
///
/// - Creates LinkInstances from configuration
/// - Stores them (type-erased) for ownership
/// - Provides handles for processors
/// - Handles disconnection and cleanup
pub struct LinkInstanceManager {
    /// Metadata storage for links
    metadata: HashMap<LinkId, LinkMetadata>,

    /// Type-erased LinkInstance storage (owns the ring buffers)
    instances: HashMap<LinkId, BoxedLinkInstance>,

    /// Index: source link port → list of link IDs
    source_index: HashMap<LinkPortAddress, Vec<LinkId>>,

    /// Index: dest link port → link ID (enforces 1-to-1 at destination)
    dest_index: HashMap<LinkPortAddress, LinkId>,
}

impl LinkInstanceManager {
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            instances: HashMap::new(),
            source_index: HashMap::new(),
            dest_index: HashMap::new(),
        }
    }

    /// Create a LinkInstance and return data writer/reader for wiring.
    ///
    /// Creates a `LinkInstance` (which owns the ring buffer) and returns
    /// data writer/reader that can be wired into processor ports.
    ///
    /// Enforces 1-to-1 rule: destination can only have one link.
    pub fn create_link_instance<T: LinkPortMessage + 'static>(
        &mut self,
        source: LinkPortAddress,
        dest: LinkPortAddress,
        capacity: usize,
        link_id: LinkId,
    ) -> Result<(LinkOutputDataWriter<T>, LinkInputDataReader<T>)> {
        // Enforce 1-to-1: Check if dest already linked
        if self.dest_index.contains_key(&dest) {
            return Err(StreamError::Link(format!(
                "Destination link port {} already has a link (1-to-1 rule)",
                dest.full_address()
            )));
        }
        let type_id = TypeId::of::<T>();

        // Create LinkInstance
        let instance = LinkInstance::<T>::new(link_id.clone(), capacity);

        // Get data writer/reader before storing
        let output_data_writer = instance.create_link_output_data_writer();
        let input_data_reader = instance.create_link_input_data_reader();

        // Store metadata
        let link_metadata = LinkMetadata {
            id: link_id.clone(),
            source: source.clone(),
            dest: dest.clone(),
            type_id,
            type_name: std::any::type_name::<T>(),
            capacity,
        };
        self.metadata.insert(link_id.clone(), link_metadata);

        // Store type-erased instance (this owns the ring buffer)
        self.instances.insert(link_id.clone(), Box::new(instance));

        // Update indices
        self.source_index
            .entry(source)
            .or_default()
            .push(link_id.clone());
        self.dest_index.insert(dest, link_id.clone());

        Ok((output_data_writer, input_data_reader))
    }

    /// Disconnect and remove a link by ID.
    ///
    /// Removes the LinkInstance, causing all handles to gracefully degrade.
    /// Returns the source and destination addresses for cleanup.
    pub fn disconnect(&mut self, id: LinkId) -> Option<(LinkPortAddress, LinkPortAddress)> {
        if let Some(link_metadata) = self.metadata.remove(&id) {
            let source = link_metadata.source.clone();
            let dest = link_metadata.dest.clone();

            // Remove LinkInstance - this drops the Arc, handles degrade gracefully
            // Ring buffer memory is freed when Arc refcount hits zero
            self.instances.remove(&id);

            // Clean up indices
            for (_, ids) in self.source_index.iter_mut() {
                ids.retain(|lid| lid != &id);
            }
            self.dest_index.retain(|_, lid| lid != &id);

            Some((source, dest))
        } else {
            None
        }
    }

    /// Get total link count.
    pub fn link_count(&self) -> usize {
        self.metadata.len()
    }

    /// Check if a destination link port is already linked.
    pub fn is_dest_linked(&self, dest: &LinkPortAddress) -> bool {
        self.dest_index.contains_key(dest)
    }

    /// Get all link IDs.
    pub fn all_links(&self) -> Vec<LinkId> {
        self.metadata.keys().cloned().collect()
    }

    /// Get the number of active LinkInstances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Check if a link has an active LinkInstance.
    pub fn has_instance(&self, id: &LinkId) -> bool {
        self.instances.contains_key(id)
    }

    /// Get link metadata by ID.
    pub fn get_metadata(&self, id: &LinkId) -> Option<(&LinkPortAddress, &LinkPortAddress)> {
        self.metadata.get(id).map(|m| (&m.source, &m.dest))
    }

    /// Get the link ID for a destination port.
    pub fn get_link_id_by_dest(&self, dest: &LinkPortAddress) -> Option<&LinkId> {
        self.dest_index.get(dest)
    }
}

impl Default for LinkInstanceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::frames::AudioChannelCount;
    use crate::core::links::graph::link_id::__private::new_unchecked;

    #[test]
    fn test_create_link_instance() {
        use crate::core::frames::AudioFrame;

        let mut manager = LinkInstanceManager::new();
        let source = LinkPortAddress::new("proc1", "out");
        let dest = LinkPortAddress::new("proc2", "in");
        let link_id = new_unchecked("test_link");

        let result = manager.create_link_instance::<AudioFrame>(
            source.clone(),
            dest.clone(),
            4,
            link_id.clone(),
        );
        assert!(result.is_ok());

        let (output_data_writer, input_data_reader) = result.unwrap();

        // Verify link was registered
        assert!(manager.is_dest_linked(&dest));
        assert_eq!(manager.link_count(), 1);
        assert!(manager.has_instance(&link_id));
        assert_eq!(manager.instance_count(), 1);

        // Verify data writer/reader work
        assert!(output_data_writer.is_connected());
        assert!(input_data_reader.is_connected());

        // Write and read through data writer/reader
        let frame = AudioFrame::new(vec![0.0; 480], AudioChannelCount::One, 0, 0, 48000);
        assert!(output_data_writer.write(frame));
        assert!(input_data_reader.read().is_some());
    }

    #[test]
    fn test_link_instance_graceful_degradation() {
        use crate::core::frames::{AudioChannelCount, AudioFrame};

        let mut manager = LinkInstanceManager::new();
        let source = LinkPortAddress::new("proc1", "out");
        let dest = LinkPortAddress::new("proc2", "in");
        let link_id = new_unchecked("test_link");

        let (output_data_writer, input_data_reader) = manager
            .create_link_instance::<AudioFrame>(source, dest, 4, link_id.clone())
            .unwrap();

        // Data writer/reader are connected
        assert!(output_data_writer.is_connected());
        assert!(input_data_reader.is_connected());

        // Disconnect the link
        manager.disconnect(link_id);

        // Data writer/reader gracefully degrade
        assert!(!output_data_writer.is_connected());
        assert!(!input_data_reader.is_connected());

        // Write silently fails (doesn't crash)
        let frame = AudioFrame::new(vec![0.0; 480], AudioChannelCount::One, 0, 0, 48000);
        assert!(!output_data_writer.write(frame));

        // Read returns None
        assert!(input_data_reader.read().is_none());
    }

    #[test]
    fn test_one_to_one_enforcement() {
        use crate::core::frames::AudioFrame;

        let mut manager = LinkInstanceManager::new();
        let source1 = LinkPortAddress::new("proc1", "out");
        let source2 = LinkPortAddress::new("proc2", "out");
        let dest = LinkPortAddress::new("proc3", "in");
        let link_id1 = new_unchecked("link1");
        let link_id2 = new_unchecked("link2");

        // First link should succeed
        let result1 =
            manager.create_link_instance::<AudioFrame>(source1, dest.clone(), 4, link_id1);
        assert!(result1.is_ok());

        // Verify destination is linked
        assert!(manager.is_dest_linked(&dest));

        // Second link to same destination should fail (1-to-1 rule)
        let result2 = manager.create_link_instance::<AudioFrame>(source2, dest, 4, link_id2);
        assert!(result2.is_err());

        if let Err(e) = result2 {
            assert!(matches!(e, StreamError::Link(_)));
        }
    }

    #[test]
    fn test_multiple_outputs_allowed() {
        use crate::core::frames::AudioFrame;

        let mut manager = LinkInstanceManager::new();
        let source = LinkPortAddress::new("proc1", "out");
        let dest1 = LinkPortAddress::new("proc2", "in");
        let dest2 = LinkPortAddress::new("proc3", "in");
        let link_id1 = new_unchecked("link1");
        let link_id2 = new_unchecked("link2");

        // Source can link to multiple destinations (fan-out)
        let result1 =
            manager.create_link_instance::<AudioFrame>(source.clone(), dest1, 4, link_id1);
        assert!(result1.is_ok());

        let result2 = manager.create_link_instance::<AudioFrame>(source, dest2, 4, link_id2);
        assert!(result2.is_ok());

        assert_eq!(manager.link_count(), 2);
    }

    #[test]
    fn test_link_count() {
        use crate::core::frames::AudioFrame;

        let mut manager = LinkInstanceManager::new();
        assert_eq!(manager.link_count(), 0);

        let source = LinkPortAddress::new("proc1", "out");
        let dest = LinkPortAddress::new("proc2", "in");
        let link_id = new_unchecked("test_link");

        manager
            .create_link_instance::<AudioFrame>(source, dest, 4, link_id)
            .unwrap();
        assert_eq!(manager.link_count(), 1);
    }
}
