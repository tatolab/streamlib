// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.

use std::cell::UnsafeCell;
use std::collections::HashMap;

use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use serde::Serialize;

use super::FramePayload;
use crate::core::error::{Result, StreamError};
use crate::core::media_clock::MediaClock;

/// Mapping from output port name to (schema_name, dest_port_name).
type PortSchemaMap = HashMap<String, (String, String)>;

/// Thread-local publisher wrapper.
///
/// # Safety
/// This wrapper is safe to send between threads because:
/// 1. The Publisher is only ever set AFTER the processor is spawned on its execution thread
/// 2. Once set, the Publisher is only accessed from that same thread
/// 3. The wrapper starts with `None` and is populated during wiring on the target thread
struct SendablePublisher(UnsafeCell<Option<Publisher<ipc::Service, FramePayload, ()>>>);

// SAFETY: The Publisher is only accessed from a single thread after being set.
// The processor lifecycle ensures that:
// 1. OutputWriter is created with publisher = None (safe to send)
// 2. After spawn, the processor is on its execution thread
// 3. set_publisher() is called from that execution thread during wiring
// 4. All subsequent access is from the same thread
unsafe impl Send for SendablePublisher {}

impl SendablePublisher {
    fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    fn set(&self, publisher: Publisher<ipc::Service, FramePayload, ()>) {
        // SAFETY: Only called from the processor's execution thread after spawn
        unsafe {
            *self.0.get() = Some(publisher);
        }
    }

    fn get(&self) -> Option<&Publisher<ipc::Service, FramePayload, ()>> {
        // SAFETY: Only called from the processor's execution thread
        unsafe { (*self.0.get()).as_ref() }
    }
}

/// Output writer that publishes frames via iceoryx2.
///
/// Each OutputWriter holds a single Publisher that sends to one downstream processor.
/// The port_schemas map stores the schema and destination port for each output port.
pub struct OutputWriter {
    publisher: SendablePublisher,
    port_schemas: PortSchemaMap,
}

impl OutputWriter {
    /// Create a new output writer without a publisher (will be set during wiring).
    pub fn new() -> Self {
        Self {
            publisher: SendablePublisher::new(),
            port_schemas: HashMap::new(),
        }
    }

    /// Create a new output writer with the given publisher.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn with_publisher(publisher: Publisher<ipc::Service, FramePayload, ()>) -> Self {
        let writer = Self {
            publisher: SendablePublisher::new(),
            port_schemas: HashMap::new(),
        };
        writer.publisher.set(publisher);
        writer
    }

    /// Set the publisher for this output writer.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn set_publisher(&self, publisher: Publisher<ipc::Service, FramePayload, ()>) {
        self.publisher.set(publisher);
    }

    /// Add a port mapping with its schema and destination port name.
    pub fn add_port(&mut self, output_port: &str, schema: &str, dest_port: &str) {
        self.port_schemas.insert(
            output_port.to_string(),
            (schema.to_string(), dest_port.to_string()),
        );
    }

    /// Write a frame to the specified output port.
    ///
    /// The frame is serialized to MessagePack, wrapped in a FramePayload with
    /// the configured schema and destination port name, then published via iceoryx2.
    pub fn write<T: Serialize>(&self, port: &str, value: &T) -> Result<()> {
        let timestamp_ns = MediaClock::now().as_nanos() as i64;
        self.write_with_timestamp(port, value, timestamp_ns)
    }

    /// Write a frame to the specified output port with an explicit timestamp.
    pub fn write_with_timestamp<T: Serialize>(
        &self,
        port: &str,
        value: &T,
        timestamp_ns: i64,
    ) -> Result<()> {
        let data = rmp_serde::to_vec(value)
            .map_err(|e| StreamError::Link(format!("Failed to serialize frame: {}", e)))?;

        let publisher = self.publisher.get().ok_or_else(|| {
            StreamError::Link("OutputWriter has no publisher configured".to_string())
        })?;

        let (schema, dest_port) = self
            .port_schemas
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

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.port_schemas.contains_key(port)
    }

    /// Get the list of configured output port names.
    pub fn ports(&self) -> impl Iterator<Item = &str> {
        self.port_schemas.keys().map(|s| s.as_str())
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::new()
    }
}
