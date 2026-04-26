// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.

use std::collections::HashMap;

use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use parking_lot::Mutex;
use serde::Serialize;

use super::{FrameHeader, FRAME_HEADER_SIZE};
use crate::core::error::{Result, StreamError};
use crate::core::media_clock::MediaClock;

/// One downstream connection: schema, destination port, publisher, and a
/// notifier into the destination's paired iceoryx2 Event service that wakes
/// the downstream processor's listener-fd-multiplexed runner loop.
struct DownstreamConnection {
    schema: String,
    dest_port: String,
    publisher: Publisher<ipc::Service, [u8], ()>,
    notifier: Notifier<ipc::Service>,
}

/// Output writer that publishes frames via iceoryx2.
///
/// Thread-safe: can be written from any thread (e.g., AVFoundation callbacks).
/// Supports fan-out: a single output port can publish to multiple downstream
/// processors, each with its own iceoryx2 publisher and destination port name.
pub struct OutputWriter {
    /// Map from output port name to downstream connections.
    connections: Mutex<HashMap<String, Vec<DownstreamConnection>>>,
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
    /// Each call adds a new publisher+notifier+routing pair. Multiple connections per
    /// output port enables fan-out (one source port → multiple destinations).
    pub fn add_connection(
        &self,
        output_port: &str,
        schema: &str,
        dest_port: &str,
        publisher: Publisher<ipc::Service, [u8], ()>,
        notifier: Notifier<ipc::Service>,
    ) {
        self.connections
            .lock()
            .entry(output_port.to_string())
            .or_default()
            .push(DownstreamConnection {
                schema: schema.to_string(),
                dest_port: dest_port.to_string(),
                publisher,
                notifier,
            });
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

        for conn in port_connections {
            let total_len = FRAME_HEADER_SIZE + data.len();
            let mut frame = vec![0u8; total_len];
            FrameHeader::new(&conn.dest_port, &conn.schema, timestamp_ns, data.len() as u32)
                .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
            frame[FRAME_HEADER_SIZE..].copy_from_slice(&data);

            let sample = conn
                .publisher
                .loan_slice_uninit(total_len)
                .map_err(|e| StreamError::Link(format!("Failed to loan slice: {:?}", e)))?;

            let sample = sample.write_from_slice(&frame);
            sample
                .send()
                .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;

            // Wake the downstream listener fd. notify() may transiently fail
            // (e.g. listener not yet created) — log and continue rather than
            // failing the publish; the data is already in shared memory and
            // the next send() will wake the listener anyway.
            if let Err(e) = conn.notifier.notify() {
                tracing::trace!(
                    "OutputWriter: notify() failed for port '{}' -> '{}': {:?}",
                    port,
                    conn.dest_port,
                    e
                );
            }
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

        for conn in port_connections {
            let total_len = FRAME_HEADER_SIZE + data.len();
            let mut frame = vec![0u8; total_len];
            FrameHeader::new(&conn.dest_port, &conn.schema, timestamp_ns, data.len() as u32)
                .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
            frame[FRAME_HEADER_SIZE..].copy_from_slice(data);

            let sample = conn
                .publisher
                .loan_slice_uninit(total_len)
                .map_err(|e| StreamError::Link(format!("Failed to loan slice: {:?}", e)))?;

            let sample = sample.write_from_slice(&frame);
            sample
                .send()
                .map_err(|e| StreamError::Link(format!("Failed to send sample: {:?}", e)))?;

            if let Err(e) = conn.notifier.notify() {
                tracing::trace!(
                    "OutputWriter: notify() failed for port '{}' -> '{}': {:?}",
                    port,
                    conn.dest_port,
                    e
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets a unique service-name prefix so parallel invocations
    /// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/output/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn write_raw_calls_notifier() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub_name = unique_suffix("pubsub");
        let notify_name = unique_suffix("notify");

        let pubsub = node
            .service_builder(&ServiceName::new(&pubsub_name).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .create()
            .unwrap();
        let _subscriber = pubsub.subscriber_builder().create().unwrap();

        let notify = node
            .service_builder(&ServiceName::new(&notify_name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = notify.notifier_builder().create().unwrap();
        let listener = notify.listener_builder().create().unwrap();

        let writer = OutputWriter::new();
        writer.add_connection("out", "schema", "in", publisher, notifier);

        // Pre-flight: the listener has no events queued.
        let mut count: usize = 0;
        listener.try_wait_all(|_| count += 1).unwrap();
        assert_eq!(count, 0);

        writer.write_raw("out", b"payload", 1234).unwrap();
        writer.write_raw("out", b"more", 5678).unwrap();

        // Notifier::notify is non-blocking; give iceoryx2 a moment to deliver
        // before draining. timed_wait_all returns as soon as the first event
        // arrives, so the deadline is generous, not the typical wait time.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while count == 0 && std::time::Instant::now() < deadline {
            listener
                .timed_wait_all(|_| count += 1, std::time::Duration::from_millis(50))
                .unwrap();
        }
        // Drain anything still pending.
        listener.try_wait_all(|_| count += 1).unwrap();
        assert!(
            count >= 1,
            "expected at least one notify after write_raw, got {}",
            count
        );
    }
}
