// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.

use std::collections::HashMap;

use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use super::FramePayload;
use crate::core::error::{Result, StreamError};
use crate::core::media_clock::MediaClock;

/// Mapping from output port name to (schema_name, dest_port_name).
type PortSchemaMap = HashMap<String, (String, String)>;

/// Output writer that publishes frames via iceoryx2.
///
/// Thread-safe: can be written from any thread (e.g., AVFoundation callbacks).
/// Each OutputWriter holds a single Publisher that sends to one downstream processor.
/// The port_schemas map stores the schema and destination port for each output port.
pub struct OutputWriter {
    publisher: Mutex<Option<Publisher<ipc::Service, FramePayload, ()>>>,
    port_schemas: RwLock<PortSchemaMap>,
}

// OutputWriter is Send + Sync via Mutex and RwLock
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Create a new output writer without a publisher (will be set during wiring).
    pub fn new() -> Self {
        Self {
            publisher: Mutex::new(None),
            port_schemas: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new output writer with the given publisher.
    pub fn with_publisher(publisher: Publisher<ipc::Service, FramePayload, ()>) -> Self {
        Self {
            publisher: Mutex::new(Some(publisher)),
            port_schemas: RwLock::new(HashMap::new()),
        }
    }

    /// Set the publisher for this output writer.
    pub fn set_publisher(&self, publisher: Publisher<ipc::Service, FramePayload, ()>) {
        *self.publisher.lock() = Some(publisher);
    }

    /// Add a port mapping with its schema and destination port name.
    pub fn add_port(&self, output_port: &str, schema: &str, dest_port: &str) {
        self.port_schemas.write().insert(
            output_port.to_string(),
            (schema.to_string(), dest_port.to_string()),
        );
    }

    /// Write a frame to the specified output port.
    ///
    /// The frame is serialized to MessagePack, wrapped in a FramePayload with
    /// the configured schema and destination port name, then published via iceoryx2.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn write<T: Serialize>(&self, port: &str, value: &T) -> Result<()> {
        let timestamp_ns = MediaClock::now().as_nanos() as i64;
        self.write_with_timestamp(port, value, timestamp_ns)
    }

    /// Write a frame to the specified output port with an explicit timestamp.
    ///
    /// Thread-safe: can be called from any thread.
    pub fn write_with_timestamp<T: Serialize>(
        &self,
        port: &str,
        value: &T,
        timestamp_ns: i64,
    ) -> Result<()> {
        let data = rmp_serde::to_vec(value)
            .map_err(|e| StreamError::Link(format!("Failed to serialize frame: {}", e)))?;

        // Lock publisher for the duration of loan + send
        let publisher_guard = self.publisher.lock();
        let publisher = publisher_guard.as_ref().ok_or_else(|| {
            StreamError::Link("OutputWriter has no publisher configured".to_string())
        })?;

        // Read lock for port schemas
        let schemas = self.port_schemas.read();
        let (schema, dest_port) = schemas
            .get(port)
            .ok_or_else(|| StreamError::Link(format!("Unknown output port: {}", port)))?;

        let payload = FramePayload::new(dest_port, schema, timestamp_ns, &data);

        // Loan a sample from the publisher and copy the payload
        let sample = publisher
            .loan_uninit()
            .map_err(|e| StreamError::Link(format!("Failed to loan sample: {:?}", e)))?;

        let sample = sample.write_payload(payload);
        sample
            .send()
            .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;

        Ok(())
    }

    /// Write raw bytes to the specified output port without serialization.
    ///
    /// The data is assumed to be pre-serialized (e.g., msgpack from a subprocess bridge).
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        let publisher_guard = self.publisher.lock();
        let publisher = publisher_guard.as_ref().ok_or_else(|| {
            StreamError::Link("OutputWriter has no publisher configured".to_string())
        })?;

        let schemas = self.port_schemas.read();
        let (schema, dest_port) = schemas
            .get(port)
            .ok_or_else(|| StreamError::Link(format!("Unknown output port: {}", port)))?;

        let payload = FramePayload::new(dest_port, schema, timestamp_ns, data);

        let sample = publisher
            .loan_uninit()
            .map_err(|e| StreamError::Link(format!("Failed to loan sample: {:?}", e)))?;

        let sample = sample.write_payload(payload);
        sample
            .send()
            .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;

        Ok(())
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.port_schemas.read().contains_key(port)
    }

    /// Get the list of configured output port names.
    pub fn port_names(&self) -> Vec<String> {
        self.port_schemas.read().keys().cloned().collect()
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::new()
    }
}
