// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.

use std::collections::HashMap;

use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use parking_lot::Mutex;
use serde::Serialize;

use super::FramePayload;
use crate::core::error::{Result, StreamError};
use crate::core::media_clock::MediaClock;

/// Output writer that publishes frames via iceoryx2.
///
/// Thread-safe: can be written from any thread (e.g., AVFoundation callbacks).
/// Supports fan-out: a single output port can publish to multiple downstream
/// processors, each with its own iceoryx2 publisher and destination port name.
pub struct OutputWriter {
    /// Map from output port name to downstream connections.
    /// Each connection is (schema, dest_port, publisher).
    connections:
        Mutex<HashMap<String, Vec<(String, String, Publisher<ipc::Service, FramePayload, ()>)>>>,
}

// OutputWriter is Send + Sync via Mutex
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Create a new output writer with no connections (populated during wiring).
    pub fn new() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Add a downstream connection for the given output port.
    ///
    /// Each call adds a new publisher+routing pair. Multiple connections per
    /// output port enables fan-out (one source port → multiple destinations).
    pub fn add_connection(
        &self,
        output_port: &str,
        schema: &str,
        dest_port: &str,
        publisher: Publisher<ipc::Service, FramePayload, ()>,
    ) {
        self.connections
            .lock()
            .entry(output_port.to_string())
            .or_default()
            .push((schema.to_string(), dest_port.to_string(), publisher));
    }

    /// Write a frame to the specified output port.
    ///
    /// The frame is serialized once, then published to all downstream connections
    /// for the given port. Each connection gets its own FramePayload with the
    /// correct destination port name for routing.
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
        let data = rmp_serde::to_vec_named(value)
            .map_err(|e| StreamError::Link(format!("Failed to serialize frame: {}", e)))?;

        let connections = self.connections.lock();
        let port_connections = connections
            .get(port)
            .ok_or_else(|| StreamError::Link(format!("Unknown output port: {}", port)))?;

        for (schema, dest_port, publisher) in port_connections {
            let payload = FramePayload::new(dest_port, schema, timestamp_ns, &data);

            let sample = publisher
                .loan_uninit()
                .map_err(|e| StreamError::Link(format!("Failed to loan sample: {:?}", e)))?;

            let sample = sample.write_payload(payload);
            sample
                .send()
                .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;
        }

        Ok(())
    }

    /// Write raw bytes to the specified output port without serialization.
    ///
    /// The data is assumed to be pre-serialized (e.g., msgpack from a subprocess bridge).
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        let connections = self.connections.lock();
        let port_connections = connections
            .get(port)
            .ok_or_else(|| StreamError::Link(format!("Unknown output port: {}", port)))?;

        for (schema, dest_port, publisher) in port_connections {
            let payload = FramePayload::new(dest_port, schema, timestamp_ns, data);

            let sample = publisher
                .loan_uninit()
                .map_err(|e| StreamError::Link(format!("Failed to loan sample: {:?}", e)))?;

            let sample = sample.write_payload(payload);
            sample
                .send()
                .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;
        }

        Ok(())
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.connections.lock().contains_key(port)
    }

    /// Get the list of configured output port names.
    pub fn port_names(&self) -> Vec<String> {
        self.connections.lock().keys().cloned().collect()
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::new()
    }
}
