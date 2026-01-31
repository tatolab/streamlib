// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! `streamlib-deno-native` share the same wire-compatible types.

pub use streamlib_ipc_types::{
    FramePayload, PortKey, SchemaName, MAX_PAYLOAD_SIZE, MAX_SCHEMA_NAME_SIZE,
};
