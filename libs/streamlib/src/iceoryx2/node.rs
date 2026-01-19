// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::node::Node;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use super::FramePayload;
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
        let node = NodeBuilder::new()
            .create::<ipc::Service>()
            .map_err(|e| StreamError::Runtime(format!("Failed to create iceoryx2 node: {:?}", e)))?;

        Ok(Self {
            inner: Arc::new(Mutex::new(node)),
        })
    }

    /// Open or create a publish-subscribe service for FramePayload.
    ///
    /// The service name should follow the format: "streamlib/{source_processor}/{dest_processor}"
    pub fn open_or_create_service(
        &self,
        service_name: &str,
    ) -> Result<Iceoryx2Service> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name
            .try_into()
            .map_err(|e| StreamError::Configuration(format!("Invalid service name '{}': {:?}", service_name, e)))?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<FramePayload>()
            .open_or_create()
            .map_err(|e| StreamError::Runtime(format!("Failed to open/create service: {:?}", e)))?;

        Ok(Iceoryx2Service { inner: service })
    }
}

/// Handle to an iceoryx2 publish-subscribe service.
pub struct Iceoryx2Service {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        FramePayload,
        (),
    >,
}

impl Iceoryx2Service {
    /// Create a publisher for this service.
    pub fn create_publisher(
        &self,
    ) -> Result<iceoryx2::port::publisher::Publisher<ipc::Service, FramePayload, ()>> {
        self.inner
            .publisher_builder()
            .create()
            .map_err(|e| StreamError::Runtime(format!("Failed to create publisher: {:?}", e)))
    }

    /// Create a subscriber for this service.
    pub fn create_subscriber(
        &self,
    ) -> Result<iceoryx2::port::subscriber::Subscriber<ipc::Service, FramePayload, ()>> {
        self.inner
            .subscriber_builder()
            .create()
            .map_err(|e| StreamError::Runtime(format!("Failed to create subscriber: {:?}", e)))
    }
}
