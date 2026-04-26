// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared iceoryx2 payload types for cross-process IPC communication.

use iceoryx2::prelude::*;

pub const MAX_PAYLOAD_SIZE: usize = 65536;
pub const MAX_SCHEMA_NAME_SIZE: usize = 128;
pub const MAX_PORT_KEY_SIZE: usize = 64;
pub const MAX_EVENT_PAYLOAD_SIZE: usize = 8192;
pub const MAX_TOPIC_KEY_SIZE: usize = 128;

/// Maximum number of upstream sources that can fan in to one destination processor.
///
/// Sized in lockstep across the per-destination iceoryx2 pub/sub service
/// (`max_publishers`) and its paired Notify service (`max_notifiers`) so both fail
/// at the same fan-in. The graph compiler validates this at compile time so that
/// overcap wiring surfaces as a named-destination configuration error instead of
/// an opaque iceoryx2 "failed to create notifier/publisher" deep inside the FFI.
pub const MAX_FANIN_PER_DESTINATION: usize = 16;

/// Size of the frame header in the `[u8]` slice wire format.
pub const FRAME_HEADER_SIZE: usize = MAX_PORT_KEY_SIZE + MAX_SCHEMA_NAME_SIZE + 8 + 4; // 204 bytes

/// Fixed-size port name for zero-copy IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct PortKey {
    len: u8,
    name: [u8; MAX_PORT_KEY_SIZE - 1],
}

impl PortKey {
    pub fn new(name: &str) -> Self {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_PORT_KEY_SIZE - 1) as u8;
        let mut key = Self {
            len,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        };
        key.name[..len as usize].copy_from_slice(&bytes[..len as usize]);
        key
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for PortKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        }
    }
}

/// Fixed-size schema name for zero-copy IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct SchemaName {
    len: u8,
    name: [u8; MAX_SCHEMA_NAME_SIZE - 1],
}

impl SchemaName {
    pub fn new(name: &str) -> Self {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_SCHEMA_NAME_SIZE - 1) as u8;
        let mut schema = Self {
            len,
            name: [0u8; MAX_SCHEMA_NAME_SIZE - 1],
        };
        schema.name[..len as usize].copy_from_slice(&bytes[..len as usize]);
        schema
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for SchemaName {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_SCHEMA_NAME_SIZE - 1],
        }
    }
}

/// Frame payload for iceoryx2 pub/sub communication.
///
/// This is the message type sent between processors via iceoryx2.
/// It includes routing information (port_key), type information (schema_name),
/// and the serialized frame data.
#[derive(Clone, Copy, ZeroCopySend)]
#[type_name("FramePayload")]
#[repr(C)]
pub struct FramePayload {
    pub port_key: PortKey,
    pub schema_name: SchemaName,
    pub timestamp_ns: i64,
    pub len: u32,
    pub data: [u8; MAX_PAYLOAD_SIZE],
}

impl FramePayload {
    /// Create a new payload with the given port, schema, and data.
    pub fn new(port: &str, schema: &str, timestamp_ns: i64, data: &[u8]) -> Self {
        let len = data.len().min(MAX_PAYLOAD_SIZE) as u32;
        let mut payload = Self {
            port_key: PortKey::new(port),
            schema_name: SchemaName::new(schema),
            timestamp_ns,
            len,
            data: [0u8; MAX_PAYLOAD_SIZE],
        };
        payload.data[..len as usize].copy_from_slice(&data[..len as usize]);
        payload
    }

    /// Get the actual data slice (excluding padding).
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the port key as a string.
    pub fn port(&self) -> &str {
        self.port_key.as_str()
    }

    /// Get the schema name as a string.
    pub fn schema(&self) -> &str {
        self.schema_name.as_str()
    }
}

impl Default for FramePayload {
    fn default() -> Self {
        Self {
            port_key: PortKey::default(),
            schema_name: SchemaName::default(),
            timestamp_ns: 0,
            len: 0,
            data: [0u8; MAX_PAYLOAD_SIZE],
        }
    }
}

impl std::fmt::Debug for FramePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramePayload")
            .field("port_key", &self.port_key.as_str())
            .field("schema_name", &self.schema_name.as_str())
            .field("timestamp_ns", &self.timestamp_ns)
            .field("len", &self.len)
            .finish()
    }
}

/// Header for slice-based iceoryx2 frame transport.
///
/// Wire format in a `[u8]` slice:
/// `[port_key: 64][schema_name: 128][timestamp_ns: 8][len: 4][data: len]`
pub struct FrameHeader {
    pub port_key: PortKey,
    pub schema_name: SchemaName,
    pub timestamp_ns: i64,
    pub len: u32,
}

impl FrameHeader {
    /// Create a new frame header.
    pub fn new(port: &str, schema: &str, timestamp_ns: i64, data_len: u32) -> Self {
        Self {
            port_key: PortKey::new(port),
            schema_name: SchemaName::new(schema),
            timestamp_ns,
            len: data_len,
        }
    }

    /// Write the header to the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    pub fn write_to_slice(&self, buf: &mut [u8]) {
        // port_key: [len: 1][name: 63] = 64 bytes
        buf[0] = self.port_key.len;
        buf[1..MAX_PORT_KEY_SIZE].copy_from_slice(&self.port_key.name);
        // schema_name: [len: 1][name: 127] = 128 bytes
        let s = MAX_PORT_KEY_SIZE;
        buf[s] = self.schema_name.len;
        buf[s + 1..s + MAX_SCHEMA_NAME_SIZE].copy_from_slice(&self.schema_name.name);
        // timestamp_ns: 8 bytes little-endian
        let t = s + MAX_SCHEMA_NAME_SIZE;
        buf[t..t + 8].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        // len: 4 bytes little-endian
        buf[t + 8..t + 12].copy_from_slice(&self.len.to_le_bytes());
    }

    /// Read a header from the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    pub fn read_from_slice(buf: &[u8]) -> Self {
        let mut port_key = PortKey::default();
        port_key.len = buf[0];
        port_key.name.copy_from_slice(&buf[1..MAX_PORT_KEY_SIZE]);

        let s = MAX_PORT_KEY_SIZE;
        let mut schema_name = SchemaName::default();
        schema_name.len = buf[s];
        schema_name
            .name
            .copy_from_slice(&buf[s + 1..s + MAX_SCHEMA_NAME_SIZE]);

        let t = s + MAX_SCHEMA_NAME_SIZE;
        let timestamp_ns = i64::from_le_bytes(buf[t..t + 8].try_into().unwrap());
        let len = u32::from_le_bytes(buf[t + 8..t + 12].try_into().unwrap());

        Self {
            port_key,
            schema_name,
            timestamp_ns,
            len,
        }
    }

    /// Read the port key string from a raw slice without parsing the full header.
    pub fn read_port_from_slice(buf: &[u8]) -> &str {
        let len = buf[0] as usize;
        std::str::from_utf8(&buf[1..1 + len]).unwrap_or("")
    }

    /// Get the port key as a string.
    pub fn port(&self) -> &str {
        self.port_key.as_str()
    }

    /// Get the schema name as a string.
    pub fn schema(&self) -> &str {
        self.schema_name.as_str()
    }
}

/// Fixed-size topic name for event pub/sub IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct TopicKey {
    len: u8,
    name: [u8; MAX_TOPIC_KEY_SIZE - 1],
}

impl TopicKey {
    pub fn new(name: &str) -> Self {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_TOPIC_KEY_SIZE - 1) as u8;
        let mut key = Self {
            len,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        };
        key.name[..len as usize].copy_from_slice(&bytes[..len as usize]);
        key
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for TopicKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        }
    }
}

/// Event payload for iceoryx2 pub/sub communication.
///
/// Carries serialized runtime events (lifecycle, graph changes, compiler, input)
/// between components via iceoryx2 shared memory.
#[derive(Clone, Copy, ZeroCopySend)]
#[type_name("EventPayload")]
#[repr(C)]
pub struct EventPayload {
    pub topic_key: TopicKey,
    pub timestamp_ns: i64,
    pub len: u32,
    pub data: [u8; MAX_EVENT_PAYLOAD_SIZE],
}

impl EventPayload {
    /// Create a new event payload with the given topic and serialized data.
    pub fn new(topic: &str, timestamp_ns: i64, data: &[u8]) -> Self {
        let len = data.len().min(MAX_EVENT_PAYLOAD_SIZE) as u32;
        let mut payload = Self {
            topic_key: TopicKey::new(topic),
            timestamp_ns,
            len,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        };
        payload.data[..len as usize].copy_from_slice(&data[..len as usize]);
        payload
    }

    /// Get the actual data slice (excluding padding).
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the topic key as a string.
    pub fn topic(&self) -> &str {
        self.topic_key.as_str()
    }
}

impl Default for EventPayload {
    fn default() -> Self {
        Self {
            topic_key: TopicKey::default(),
            timestamp_ns: 0,
            len: 0,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        }
    }
}

impl std::fmt::Debug for EventPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventPayload")
            .field("topic_key", &self.topic_key.as_str())
            .field("timestamp_ns", &self.timestamp_ns)
            .field("len", &self.len)
            .finish()
    }
}
