// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! `streamlib-deno-native` share the same wire-compatible types.

pub use streamlib_ipc_types::{
    ChannelTrustTier, DEFAULT_EXPECTED_PAYLOAD_BYTES, DEFAULT_MAX_QUEUED_MESSAGES, EventPayload,
    FRAME_HEADER_SIZE, FrameHeader, MAX_EVENT_PAYLOAD_SIZE, MAX_PUBLISHERS_PER_CHANNEL,
    MAX_TOPIC_KEY_SIZE, PortKey, RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
    SCHEMA_IDENT_WIRE_MAX_ORG_LEN, SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN, SCHEMA_IDENT_WIRE_MAX_TYPE_LEN,
    SCHEMA_IDENT_WIRE_SIZE, SchemaIdentWire, SchemaIdentWireError,
    TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES, TopicKey,
    UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
};
