// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess channel trait for direct runtime-subprocess communication.

use std::ffi::c_void;
use std::time::Duration;

use super::FrameTransportHandle;
use crate::core::error::StreamError;

/// Role of the channel participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRole {
    /// Runtime side - creates anonymous listener, registers endpoint with broker.
    Runtime,
    /// Subprocess side - connects to runtime via broker endpoint.
    Subprocess,
}

/// Channel trait for bidirectional frame transport between runtime and subprocess.
///
/// On macOS, this is implemented via XPC anonymous listeners and endpoints.
/// On Linux, this would use Unix domain sockets.
/// On Windows, this would use named pipes.
pub trait SubprocessRhiChannel: Send + Sync {
    /// Create a new channel as the runtime (host) side.
    ///
    /// Creates an anonymous XPC listener and returns the channel.
    /// The caller should then register the endpoint with the broker.
    fn create_as_runtime(runtime_id: &str) -> Result<Self, StreamError>
    where
        Self: Sized;

    /// Create a new channel as the subprocess (guest) side.
    ///
    /// Looks up the runtime endpoint from the broker and establishes
    /// a direct connection.
    fn connect_as_subprocess(runtime_id: &str) -> Result<Self, StreamError>
    where
        Self: Sized;

    /// Get the role of this channel.
    fn role(&self) -> ChannelRole;

    /// Get the XPC endpoint for this channel (runtime side only).
    ///
    /// Returns `None` if called on subprocess side.
    fn endpoint(&self) -> Option<*mut c_void>;

    /// Send a frame transport handle to the other side.
    ///
    /// The frame_id is used for correlation.
    fn send_frame(&self, handle: FrameTransportHandle, frame_id: u64) -> Result<(), StreamError>;

    /// Receive a frame transport handle from the other side.
    ///
    /// Blocks for up to `timeout` duration.
    /// Returns the handle and frame_id.
    fn recv_frame(&self, timeout: Duration) -> Result<(FrameTransportHandle, u64), StreamError>;

    /// Send a control message (non-frame data).
    fn send_control(&self, message_type: &str, payload: &[u8]) -> Result<(), StreamError>;

    /// Receive a control message with timeout.
    fn recv_control(&self, timeout: Duration) -> Result<(String, Vec<u8>), StreamError>;

    /// Check if the channel is connected.
    fn is_connected(&self) -> bool;

    /// Close the channel gracefully.
    fn close(&self);
}
