// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Frame payload types for iceoryx2 IPC communication.
//!
//! Re-exports from [`streamlib_ipc_types`] so both `streamlib` and
//! the wheel's helper-process transport share the same wire-compatible types.

pub use streamlib_ipc_types::{
    ARRAY_FRAME_ATTRIBUTE_ROW_BYTES, ArrayFrameAttributeResidency, ArrayFrameAttributeRow,
    ArrayFrameAttributeTableRefusal, ArrayFrameCpuAttributeToPack, ArrayFrameDescriptorBody,
    ChannelTrustTier, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES, DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelPayloadCacheLine, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, DataChannelSamplePayloadRefusal, DataChannelSampleUserHeader,
    GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
    MAX_PORT_KEY_SIZE, MAX_PUBLISHERS_PER_CHANNEL, MessageGroupBoundary,
    PacketsUnitCodecNameDoesNotFit, PacketsUnitDescriptorBody, PortKey,
    RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES, RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
    TRUSTED_CHANNEL_CHUNK_CEILING_BYTES, UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
    array_frame_attribute_table_of, array_frame_cpu_data_offsets,
    array_frame_payload_of_cpu_attributes, data_channel_payload_bytes_of,
    data_channel_payload_cache_lines_for,
};
