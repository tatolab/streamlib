// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::super::LinkUniqueId;
use super::super::components::{
    ComponentMap, ComponentSerializer, default_component_serializers, default_components,
};
use super::super::{GraphEdgeWithComponents, GraphWeight};
use super::LinkCapacity;
use super::{InputLinkPortRef, LinkState, OutputLinkPortRef};

/// Link in the processor graph (connection between two ports) with embedded component storage.
#[derive(Serialize, Deserialize)]
pub struct Link {
    /// Unique link identifier
    pub id: LinkUniqueId,
    /// Source endpoint (output port)
    pub source: OutputLinkPortRef,
    /// Target endpoint (input port)
    pub target: InputLinkPortRef,
    /// Ring buffer capacity for the channel.
    #[serde(default)]
    pub capacity: LinkCapacity,
    /// Current state of the link.
    #[serde(default)]
    pub state: LinkState,
    /// Runtime components (not serialized by derive, but via serialize_components).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
    /// Serializers for each inserted component type.
    #[serde(skip, default = "default_component_serializers")]
    component_serializers: Vec<ComponentSerializer>,
}

impl PartialEq for Link {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.source == other.source
            && self.target == other.target
            && self.capacity == other.capacity
            && self.state == other.state
    }
}

impl Eq for Link {}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("target", &self.target)
            .field("capacity", &self.capacity)
            .field("state", &self.state)
            // Skip: components, component_serializers (runtime-only)
            .finish()
    }
}

impl Link {
    /// Create a link joining two ports, with default capacity. ID is generated
    /// automatically.
    pub fn between(source: OutputLinkPortRef, target: InputLinkPortRef) -> Self {
        Self::between_with_capacity(source, target, LinkCapacity::default())
    }

    /// Create a link joining two ports with explicit buffer capacity. ID is
    /// generated automatically using cuid2.
    ///
    /// The endpoints arrive as references rather than as `"<node>.<port>"`
    /// text: a mesh address carries a display name a user chose, and splitting
    /// one back apart on `.` would tear any address whose display name or port
    /// carries one.
    pub fn between_with_capacity(
        source: OutputLinkPortRef,
        target: InputLinkPortRef,
        capacity: LinkCapacity,
    ) -> Self {
        Self {
            id: LinkUniqueId::new(),
            source,
            target,
            capacity,
            state: LinkState::Pending,
            components: ComponentMap::new(),
            component_serializers: Vec::new(),
        }
    }

    /// Set the link state.
    pub fn set_state(&mut self, state: LinkState) {
        self.state = state;
    }

    /// Get source endpoint reference.
    pub fn from_port(&self) -> &OutputLinkPortRef {
        &self.source
    }

    /// Get target endpoint reference.
    pub fn to_port(&self) -> &InputLinkPortRef {
        &self.target
    }
}

impl GraphWeight for Link {
    fn id(&self) -> &str {
        &self.id
    }
}

impl GraphEdgeWithComponents for Link {
    fn components(&self) -> &ComponentMap {
        &self.components
    }

    fn components_mut(&mut self) -> &mut ComponentMap {
        &mut self.components
    }

    fn component_serializers(&self) -> &[ComponentSerializer] {
        &self.component_serializers
    }

    fn component_serializers_mut(&mut self) -> &mut Vec<ComponentSerializer> {
        &mut self.component_serializers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::MeshPortAddress;

    /// A port name carrying a `.` survives into the link. The endpoints used to
    /// arrive as `"<node>.<port>"` text and be split back apart on the first
    /// `.`, which tore exactly this.
    #[test]
    fn a_port_name_carrying_a_dot_reaches_the_link_whole() {
        let link = Link::between(
            OutputLinkPortRef::new("Psrc", "video.raw"),
            InputLinkPortRef::new("Pdst", "video.in"),
        );
        assert_eq!(link.from_port().port_name(), "video.raw");
        assert_eq!(link.to_port().port_name(), "video.in");
    }

    /// A source on another runtime reaches the link as its address, rather than
    /// as a local reference built from whatever sat before the first `.`.
    #[test]
    fn a_source_on_another_runtime_reaches_the_link_as_its_address() {
        let address = MeshPortAddress::new("bench-cam-a1b2", "Camera.Source", "video")
            .expect("a legal address");
        let link = Link::between(
            OutputLinkPortRef::on_another_runtime(address.clone()),
            InputLinkPortRef::new("Pdst", "video"),
        );
        assert_eq!(link.from_port().mesh_port_address(), Some(&address));
        assert_eq!(link.from_port().processor_id_on_this_runtime(), None);
    }
}
