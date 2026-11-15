// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use iceoryx2::prelude::*;

pub const MAX_PORT_KEY_SIZE: usize = 64;

/// Error constructing a [`PortKey`] from a name that overflows the fixed
/// wire capacity.
///
/// A port / channel name longer than [`MAX_PORT_KEY_SIZE`] `- 1` bytes is refused
/// rather than clipped: a clipped name would route frames to a different port
/// than the one the author named. Names crossing this
/// boundary have already passed the charset + length grammar in
/// the engine's `iceoryx2::validate_channel_name`; this guard is the wire-level
/// backstop that makes truncation unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKeyError {
    /// The UTF-8 name is `len` bytes, past the fixed `max`-byte capacity.
    TooLong { len: usize, max: usize },
}

impl std::fmt::Display for PortKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(
                f,
                "port key name is {len} bytes, exceeding the fixed wire capacity of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for PortKeyError {}

/// Fixed-size port name for zero-copy IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct PortKey {
    len: u8,
    name: [u8; MAX_PORT_KEY_SIZE - 1],
}

// The `len` field is one wire byte, so the name capacity must stay addressable
// by it: widening `MAX_PORT_KEY_SIZE` past 256 would silently wrap the lengths
// this type narrows to `u8` rather than fail to compile.
const _: () = assert!(PortKey::MAX_NAME_BYTES <= u8::MAX as usize);

impl PortKey {
    /// Maximum UTF-8 byte length a port / channel name may occupy on the wire
    /// (the fixed `name` field is [`MAX_PORT_KEY_SIZE`] `- 1` bytes).
    pub const MAX_NAME_BYTES: usize = MAX_PORT_KEY_SIZE - 1;

    /// Construct a [`PortKey`], rejecting an over-length name.
    ///
    /// A name past [`PortKey::MAX_NAME_BYTES`] is a [`PortKeyError::TooLong`]
    /// rather than a silent truncation — see [`PortKeyError`].
    pub fn new(name: &str) -> Result<Self, PortKeyError> {
        let bytes = name.as_bytes();
        if bytes.len() > Self::MAX_NAME_BYTES {
            return Err(PortKeyError::TooLong {
                len: bytes.len(),
                max: Self::MAX_NAME_BYTES,
            });
        }
        let mut key = Self {
            len: bytes.len() as u8,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        };
        key.name[..bytes.len()].copy_from_slice(bytes);
        Ok(key)
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

#[cfg(test)]
mod tests;
