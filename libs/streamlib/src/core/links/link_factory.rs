// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link factory for creating link instances.

use std::any::Any;
use std::sync::Arc;

use super::runtime::BoxedLinkInstance;
use crate::core::graph::{LinkCapacity, LinkTypeInfoComponent, LinkUniqueId};
use crate::core::schema_registry::SCHEMA_REGISTRY;
use crate::core::Result;

/// Result of creating a link instance.
pub struct LinkInstanceCreationResult {
    /// The boxed link instance (owns the ring buffer).
    pub instance: BoxedLinkInstance,
    /// Type info for the link.
    pub type_info: LinkTypeInfoComponent,
    /// Data writer for the source processor (boxed for type erasure).
    pub data_writer: Box<dyn Any + Send>,
    /// Data reader for the destination processor (boxed for type erasure).
    pub data_reader: Box<dyn Any + Send>,
}

/// Factory for creating link instances.
///
/// Creates `LinkInstance` objects based on schema name.
pub trait LinkFactoryDelegate: Send + Sync {
    /// Create a link instance for the given schema name and link ID.
    /// Returns pre-wrapped data writers/readers that include the link ID.
    fn create_by_schema(
        &self,
        schema_name: &str,
        capacity: LinkCapacity,
        link_id: &LinkUniqueId,
    ) -> Result<LinkInstanceCreationResult>;
}

/// Default link factory implementation.
///
/// Creates ring-buffer-based `LinkInstance` objects using the schema registry.
pub struct DefaultLinkFactory;

impl DefaultLinkFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultLinkFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkFactoryDelegate for DefaultLinkFactory {
    fn create_by_schema(
        &self,
        schema_name: &str,
        capacity: LinkCapacity,
        link_id: &LinkUniqueId,
    ) -> Result<LinkInstanceCreationResult> {
        SCHEMA_REGISTRY.create_link_instance(schema_name, capacity, link_id)
    }
}

// Blanket impl for Arc
impl<T: LinkFactoryDelegate + ?Sized> LinkFactoryDelegate for Arc<T> {
    fn create_by_schema(
        &self,
        schema_name: &str,
        capacity: LinkCapacity,
        link_id: &LinkUniqueId,
    ) -> Result<LinkInstanceCreationResult> {
        (**self).create_by_schema(schema_name, capacity, link_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_factory_audio() {
        let factory = DefaultLinkFactory::new();
        let link_id = LinkUniqueId::from("test-audio-link");

        let result = factory
            .create_by_schema("AudioFrame", 4.into(), &link_id)
            .expect("should create audio link");

        assert_eq!(result.type_info.capacity, 4.into());
        assert!(result.type_info.type_name.contains("AudioFrame"));
    }

    #[test]
    fn test_default_factory_video() {
        let factory = DefaultLinkFactory::new();
        let link_id = LinkUniqueId::from("test-video-link");

        let result = factory
            .create_by_schema("VideoFrame", LinkCapacity::from(8), &link_id)
            .expect("should create video link");

        assert_eq!(result.type_info.capacity, 8.into());
        assert!(result.type_info.type_name.contains("VideoFrame"));
    }

    #[test]
    fn test_default_factory_unknown_schema() {
        let factory = DefaultLinkFactory::new();
        let link_id = LinkUniqueId::from("test-unknown-link");

        // Unregistered schema should fail
        let result = factory.create_by_schema("UnknownSchema", LinkCapacity::from(16), &link_id);
        assert!(result.is_err());
    }
}
