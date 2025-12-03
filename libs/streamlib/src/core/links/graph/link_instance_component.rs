//! ECS components for link runtime state.

use std::any::TypeId;

use super::super::runtime::BoxedLinkInstance;

/// ECS component storing the link instance (ring buffer ownership).
///
/// When this component is removed from an entity, the ring buffer is dropped
/// and all handles (data writers/readers) gracefully degrade.
pub struct LinkInstanceComponent(pub BoxedLinkInstance);

/// ECS component storing link type information for debugging and validation.
pub struct LinkTypeInfoComponent {
    /// TypeId of the message type flowing through this link.
    pub type_id: TypeId,
    /// Human-readable type name.
    pub type_name: &'static str,
    /// Ring buffer capacity.
    pub capacity: usize,
}

impl LinkTypeInfoComponent {
    /// Create new link type info.
    pub fn new<T: 'static>(capacity: usize) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            capacity,
        }
    }
}
