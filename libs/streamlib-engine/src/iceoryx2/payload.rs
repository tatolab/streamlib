// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! `streamlib-deno-native` share the same wire-compatible types.

pub use streamlib_ipc_types::{
    EventPayload, FrameHeader, FramePayload, PortKey, SchemaIdentWire, SchemaIdentWireError,
    TopicKey, DEFAULT_MAX_QUEUED_MESSAGES, FRAME_HEADER_SIZE, MAX_EVENT_PAYLOAD_SIZE,
    MAX_FANIN_PER_DESTINATION, MAX_PAYLOAD_SIZE, MAX_TOPIC_KEY_SIZE,
    SCHEMA_IDENT_WIRE_MAX_ORG_LEN, SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN,
    SCHEMA_IDENT_WIRE_MAX_TYPE_LEN, SCHEMA_IDENT_WIRE_SIZE,
};
