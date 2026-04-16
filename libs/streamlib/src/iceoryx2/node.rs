// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::node::Node;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use super::{EventPayload, FRAME_HEADER_SIZE};
use crate::core::error::{Result, StreamError};

/// Thread-safe wrapper for iceoryx2 Node.
///
/// The Node is created once per runtime and shared across all processors.
/// Services, Publishers, and Subscribers are created through this Node.
#[derive(Clone)]
pub struct Iceoryx2Node {
    inner: Arc<Mutex<Node<ipc::Service>>>,
}

impl Iceoryx2Node {
    /// Create a new iceoryx2 Node.
    pub fn new() -> Result<Self> {
        let node = NodeBuilder::new().create::<ipc::Service>().map_err(|e| {
            StreamError::Runtime(format!("Failed to create iceoryx2 node: {:?}", e))
        })?;

        Ok(Self {
            inner: Arc::new(Mutex::new(node)),
        })
    }

    /// Open or create a publish-subscribe service for EventPayload.
    ///
    /// The service name should follow the format: "streamlib/{runtime_id}/events/{topic}"
    pub fn open_or_create_event_service(&self, service_name: &str) -> Result<Iceoryx2EventService> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            StreamError::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<EventPayload>()
            .max_publishers(16)
            .subscriber_max_buffer_size(64)
            .open_or_create()
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to open/create event service: {:?}", e))
            })?;

        Ok(Iceoryx2EventService { inner: service })
    }

    /// Open or create a publish-subscribe service for `[u8]` slices.
    ///
    /// The service name should follow the format: "streamlib/{source_processor}/{dest_processor}"
    pub fn open_or_create_service(&self, service_name: &str) -> Result<Iceoryx2Service> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            StreamError::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .max_publishers(16)
            .subscriber_max_buffer_size(16)
            .open_or_create()
            .map_err(|e| StreamError::Runtime(format!("Failed to open/create service: {:?}", e)))?;

        Ok(Iceoryx2Service { inner: service })
    }
}

/// Handle to an iceoryx2 publish-subscribe service for `[u8]` slices.
pub struct Iceoryx2Service {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        [u8],
        (),
    >,
}

impl Iceoryx2Service {
    /// Create a publisher for this service.
    ///
    /// `max_payload_bytes` sets the per-slot shared memory capacity (data only, header is added
    /// internally). Use [`crate::core::embedded_schemas::max_payload_bytes_for_schema`] to derive
    /// this value from the output port's schema declaration.
    pub fn create_publisher(
        &self,
        max_payload_bytes: usize,
    ) -> Result<iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>> {
        self.inner
            .publisher_builder()
            .initial_max_slice_len(max_payload_bytes + FRAME_HEADER_SIZE)
            .create()
            .map_err(|e| StreamError::Runtime(format!("Failed to create publisher: {:?}", e)))
    }

    /// Create a subscriber for this service.
    pub fn create_subscriber(
        &self,
    ) -> Result<iceoryx2::port::subscriber::Subscriber<ipc::Service, [u8], ()>> {
        self.inner
            .subscriber_builder()
            .buffer_size(16)
            .create()
            .map_err(|e| StreamError::Runtime(format!("Failed to create subscriber: {:?}", e)))
    }
}

/// Handle to an iceoryx2 publish-subscribe service for events.
pub struct Iceoryx2EventService {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        EventPayload,
        (),
    >,
}

impl Iceoryx2EventService {
    /// Create a publisher for this event service.
    pub fn create_publisher(
        &self,
    ) -> Result<iceoryx2::port::publisher::Publisher<ipc::Service, EventPayload, ()>> {
        self.inner
            .publisher_builder()
            .create()
            .map_err(|e| StreamError::Runtime(format!("Failed to create event publisher: {:?}", e)))
    }

    /// Create a subscriber for this event service.
    pub fn create_subscriber(
        &self,
    ) -> Result<iceoryx2::port::subscriber::Subscriber<ipc::Service, EventPayload, ()>> {
        self.inner
            .subscriber_builder()
            .buffer_size(64)
            .create()
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to create event subscriber: {:?}", e))
            })
    }
}
