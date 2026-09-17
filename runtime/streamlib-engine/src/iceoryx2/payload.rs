// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! the wheel's helper-process transport share the same wire-compatible types.

pub use streamlib_ipc_types::{
    ChannelTrustTier, DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelBagSequenceNumberUserHeader,
    FRAME_HEADER_PAYLOAD_LEN_SIZE, FRAME_HEADER_SIZE, FRAME_HEADER_TIMESTAMP_NS_SIZE, FrameHeader,
    MAX_PORT_KEY_SIZE, MAX_PUBLISHERS_PER_CHANNEL, PortKey,
    RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL, TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
    UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
};
