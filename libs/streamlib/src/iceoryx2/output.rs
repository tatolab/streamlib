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

/// A local iceoryx2 downstream connection (schema, dest_port, publisher).
type LocalIceoryx2Connection = (String, String, Publisher<ipc::Service, FramePayload, ()>);

/// A MoQ remote downstream connection (schema, publish_session).
#[cfg(feature = "moq")]
type MoqRemoteConnection = (String, std::sync::Arc<Mutex<crate::core::streaming::MoqPublishSession>>);

/// Output writer that publishes frames via iceoryx2 and optionally MoQ.
///
/// Thread-safe: can be written from any thread (e.g., AVFoundation callbacks).
/// Supports fan-out: a single output port can publish to multiple downstream
/// processors (iceoryx2) and/or MoQ relays simultaneously.
pub struct OutputWriter {
    /// Map from output port name to local iceoryx2 downstream connections.
    local_connections: Mutex<HashMap<String, Vec<LocalIceoryx2Connection>>>,

    /// Map from output port name to remote MoQ downstream connections.
    #[cfg(feature = "moq")]
    moq_connections: Mutex<HashMap<String, Vec<MoqRemoteConnection>>>,
}

// OutputWriter is Send + Sync via Mutex
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Create a new output writer with no connections (populated during wiring).
    pub fn new() -> Self {
        Self {
            local_connections: Mutex::new(HashMap::new()),
            #[cfg(feature = "moq")]
            moq_connections: Mutex::new(HashMap::new()),
        }
    }

    /// Add a local iceoryx2 downstream connection for the given output port.
    pub fn add_connection(
        &self,
        output_port: &str,
        schema: &str,
        dest_port: &str,
        publisher: Publisher<ipc::Service, FramePayload, ()>,
    ) {
        self.local_connections
            .lock()
            .entry(output_port.to_string())
            .or_default()
            .push((schema.to_string(), dest_port.to_string(), publisher));
    }

    /// Add a MoQ remote downstream connection for the given output port.
    #[cfg(feature = "moq")]
    pub fn add_moq_connection(
        &self,
        output_port: &str,
        schema: &str,
        moq_publish_session: std::sync::Arc<Mutex<crate::core::streaming::MoqPublishSession>>,
    ) {
        self.moq_connections
            .lock()
            .entry(output_port.to_string())
            .or_default()
            .push((schema.to_string(), moq_publish_session));
    }

    /// Write a frame to the specified output port.
    ///
    /// The frame is serialized once, then published to all downstream connections
    /// (both local iceoryx2 and remote MoQ) for the given port.
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
        let data = rmp_serde::to_vec_named(value)
            .map_err(|e| StreamError::Link(format!("Failed to serialize frame: {}", e)))?;

        self.publish_to_local_connections(port, &data, timestamp_ns)?;

        #[cfg(feature = "moq")]
        self.publish_to_moq_connections(port, &data)?;

        Ok(())
    }

    /// Write raw bytes to the specified output port without serialization.
    ///
    /// The data is assumed to be pre-serialized (e.g., msgpack from a subprocess bridge).
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        self.publish_to_local_connections(port, data, timestamp_ns)?;

        #[cfg(feature = "moq")]
        self.publish_to_moq_connections(port, data)?;

        Ok(())
    }

    fn publish_to_local_connections(
        &self,
        port: &str,
        data: &[u8],
        timestamp_ns: i64,
    ) -> Result<()> {
        let connections = self.local_connections.lock();
        if let Some(port_connections) = connections.get(port) {
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
        }
        Ok(())
    }

    #[cfg(feature = "moq")]
    fn publish_to_moq_connections(&self, port: &str, data: &[u8]) -> Result<()> {
        let moq_connections = self.moq_connections.lock();
        if let Some(moq_port_connections) = moq_connections.get(port) {
            for (schema, moq_session) in moq_port_connections {
                // Use schema_name as the MoQ track name
                let mut session = moq_session.lock();
                // For now, treat every frame as a non-keyframe.
                // The caller should use publish_frame_to_moq for keyframe control.
                if let Err(e) = session.publish_frame(schema, data, false) {
                    tracing::warn!(port, schema, %e, "MoQ publish failed");
                }
            }
        }
        Ok(())
    }

    /// Check if a port is configured (either local or MoQ).
    pub fn has_port(&self, port: &str) -> bool {
        let has_local = self.local_connections.lock().contains_key(port);

        #[cfg(feature = "moq")]
        let has_moq = self.moq_connections.lock().contains_key(port);
        #[cfg(not(feature = "moq"))]
        let has_moq = false;

        has_local || has_moq
    }

    /// Get the list of configured output port names (union of local and MoQ).
    pub fn port_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.local_connections.lock().keys().cloned().collect();

        #[cfg(feature = "moq")]
        {
            for key in self.moq_connections.lock().keys() {
                if !names.contains(key) {
                    names.push(key.clone());
                }
            }
        }

        names
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::new()
    }
}
