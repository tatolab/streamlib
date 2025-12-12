// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link factory for creating link instances.

use std::any::Any;
use std::sync::Arc;

use super::runtime::{BoxedLinkInstance, LinkInstance};
use super::traits::LinkPortType;
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{LinkCapacity, LinkTypeInfoComponent};
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
/// Mirrors `FactoryDelegate` for processors. Creates `LinkInstance` objects
/// based on the port type (Audio, Video, Data).
pub trait LinkFactoryDelegate: Send + Sync {
    /// Create a link instance for the given port type.
    fn create(
        &self,
        port_type: LinkPortType,
        capacity: LinkCapacity,
    ) -> Result<LinkInstanceCreationResult>;
}

/// Default link factory implementation.
///
/// Creates ring-buffer-based `LinkInstance` objects for each port type.
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
    fn create(
        &self,
        port_type: LinkPortType,
        capacity: LinkCapacity,
    ) -> Result<LinkInstanceCreationResult> {
        match port_type {
            LinkPortType::Audio => create_typed_instance::<AudioFrame>(capacity),
            LinkPortType::Video => create_typed_instance::<VideoFrame>(capacity),
            LinkPortType::Data => create_typed_instance::<DataFrame>(capacity),
        }
    }
}

/// Create a typed link instance and return boxed components.
fn create_typed_instance<T>(capacity: LinkCapacity) -> Result<LinkInstanceCreationResult>
where
    T: crate::core::LinkPortMessage + 'static,
{
    let instance = LinkInstance::<T>::new(capacity);
    let data_writer = instance.create_link_output_data_writer();
    let data_reader = instance.create_link_input_data_reader();

    Ok(LinkInstanceCreationResult {
        instance: Box::new(instance),
        type_info: LinkTypeInfoComponent::new::<T>(capacity),
        data_writer: Box::new(data_writer),
        data_reader: Box::new(data_reader),
    })
}

// Blanket impl for Arc
impl<T: LinkFactoryDelegate + ?Sized> LinkFactoryDelegate for Arc<T> {
    fn create(
        &self,
        port_type: LinkPortType,
        capacity: LinkCapacity,
    ) -> Result<LinkInstanceCreationResult> {
        (**self).create(port_type, capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_factory_audio() {
        let factory = DefaultLinkFactory::new();

        let result = factory
            .create(LinkPortType::Audio, 4.into())
            .expect("should create audio link");

        assert_eq!(result.type_info.capacity, 4.into());
        assert!(result.type_info.type_name.contains("AudioFrame"));
    }

    #[test]
    fn test_default_factory_video() {
        let factory = DefaultLinkFactory::new();

        let result = factory
            .create(LinkPortType::Video, LinkCapacity::from(8))
            .expect("should create video link");

        assert_eq!(result.type_info.capacity, 8.into());
        assert!(result.type_info.type_name.contains("VideoFrame"));
    }

    #[test]
    fn test_default_factory_data() {
        let factory = DefaultLinkFactory::new();

        let result = factory
            .create(LinkPortType::Data, LinkCapacity::from(16))
            .expect("should create data link");

        assert_eq!(result.type_info.capacity, 16.into());
        assert!(result.type_info.type_name.contains("DataFrame"));
    }
}
