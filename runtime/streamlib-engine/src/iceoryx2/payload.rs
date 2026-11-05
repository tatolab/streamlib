// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! the wheel's helper-process transport share the same wire-compatible types.

pub use streamlib_ipc_types::{
    ChannelTrustTier, DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES, DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelPayloadCacheLine, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, DataChannelSamplePayloadRefusal, DataChannelSampleUserHeader,
    GpuPoolSlotFrameDescriptorBody, MAX_PORT_KEY_SIZE, MAX_PUBLISHERS_PER_CHANNEL,
    MessageGroupBoundary, PortKey, RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
    TRUSTED_CHANNEL_CHUNK_CEILING_BYTES, UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
    data_channel_payload_bytes_of, data_channel_payload_cache_lines_for,
};
