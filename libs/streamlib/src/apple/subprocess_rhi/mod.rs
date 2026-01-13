// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS XPC-based subprocess RHI implementation.
//!
//! This module provides zero-copy cross-process frame transport using:
//! - XPC anonymous listeners for direct runtime-subprocess connections
//! - IOSurface XPC for GPU frame sharing (VideoFrame)
//! - xpc_shmem for CPU frame sharing (AudioFrame/DataFrame)
//!
//! The broker service is managed separately via `streamlib broker install`.

mod block_helpers;
mod xpc_broker;
mod xpc_channel;
mod xpc_frame_transport;

#[cfg(test)]
mod tests;

pub use xpc_broker::{XpcBroker, BROKER_SERVICE_NAME};
pub use xpc_channel::XpcChannel;
pub use xpc_frame_transport::{release_frame_transport_handle, XpcFrameTransport};
