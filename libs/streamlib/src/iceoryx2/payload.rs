// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.

use iceoryx2::prelude::*;

pub const MAX_PAYLOAD_SIZE: usize = 32768;
pub const MAX_SCHEMA_NAME_SIZE: usize = 128;
pub const MAX_PORT_KEY_SIZE: usize = 64;

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
