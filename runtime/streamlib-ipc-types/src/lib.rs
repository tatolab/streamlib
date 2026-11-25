// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Shared iceoryx2 payload types for cross-process IPC communication.
//!
//! A [`DataChannelSampleUserHeader`] rides every channel sample in its
//! iceoryx2 user header — numbering, stamp, length, group flag and a
//! [`DataChannelPayloadDescriptor`] — and the payload area carries nothing but
//! payload. Nothing on this wire names a message type, so a consumer handed a
//! payload it cannot read discovers that as a decode failure of the payload
//! itself, at its own read.
//!
//! Alongside the payload types, this crate owns the channel rules the host
//! and every language native must agree on byte-for-byte or silently drift:
//! [`decide_channel_egress_admission`] with its
//! [`emit_channel_egress_admission_tracing`] diagnostics (so the crate does
//! emit `tracing`).

mod bags_a_gap_in_the_numbering_says_were_lost;
mod bundle_frame_payload;
mod channel_chunk_ceilings;
mod channel_egress_admission;
mod channel_service_slot_limits;
mod data_channel_sample;
mod engine_owned_iceoryx2_domain;
mod gpu_pool_claim_table;
#[cfg(all(target_os = "macos", not(loom)))]
mod gpu_pool_claim_table_mach_memory_entry;
#[cfg(all(target_os = "linux", not(loom)))]
mod gpu_pool_claim_table_memfd;
mod gpu_pool_slot_frame_descriptor_body;
mod helper_process_loss_count_board;
mod payload_descriptor_bodies;
mod port_key;
mod reserved_slot_reader;
mod stream_clock_board;
pub use bags_a_gap_in_the_numbering_says_were_lost::BagsAGapInTheNumberingSaysWereLost;
pub use bundle_frame_payload::{
    BUNDLE_FRAME_BODY_USED_BYTES, BUNDLE_FRAME_ROW_BYTES, BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET,
    BUNDLE_FRAME_ROW_DESCRIPTOR_KIND_OFFSET, BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET,
    BUNDLE_FRAME_ROW_HOLDS_OFFSET, BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES,
    BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET, BUNDLE_FRAME_ROW_PAYLOAD_ALIGNMENT_BYTES,
    BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET, BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET,
    BundleFrameDescriptorBody, BundleFrameRefusal, BundleFrameRow, BundleFrameRowHolds,
    BundleFrameRowToPack, bundle_frame_of_rows, bundle_frame_row_table_of,
};
pub use channel_chunk_ceilings::{
    DEFAULT_EXPECTED_PAYLOAD_BYTES, TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES, chunk_bytes_a_ceiling_sized_bag_takes,
    iceoryx2_sample_bytes_for_a_channel_frame, largest_channel_frame_bytes_under_a_chunk_ceiling,
};
pub use channel_egress_admission::{
    ChannelEgressAdmission, ChannelSegmentGrowth, ChannelTrustTier,
    decide_channel_egress_admission, emit_channel_egress_admission_tracing,
};
pub use channel_service_slot_limits::{
    MAX_DESTINATIONS_PER_CHANNEL, MAX_INBOUND_LINKS_PER_DESTINATION, MAX_PUBLISHERS_PER_CHANNEL,
    RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
};
pub use data_channel_sample::{
    DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES, DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES,
    DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP, DataChannelPayloadCacheLine,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    DataChannelSamplePayloadRefusal, DataChannelSampleUserHeader, MessageGroupBoundary,
    data_channel_payload_bytes_of, data_channel_payload_cache_lines_for,
};
pub use engine_owned_iceoryx2_domain::{
    ChannelDataServiceLoanedSample, ChannelDataServicePortFactory, ChannelDataServicePublisher,
    ChannelDataServiceSample, ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError,
    ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES, Iceoryx2DomainRefusal,
    bind_an_iceoryx2_listener_outside_every_child_process_start, channel_data_service_builder,
    create_iceoryx2_node_in_domain, create_the_reserved_slot_subscriber_of,
    engine_owned_iceoryx2_prefix_for_this_user, iceoryx2_config_for_domain,
};
pub use gpu_pool_claim_table::{
    GPU_POOL_CLAIM_TABLE_BYTES, GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS,
    GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT, GPU_POOL_CLAIM_TABLE_ROW_BYTES,
    GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET, GPU_POOL_CLAIM_TABLE_ROW_LEASE_NUMBER_OFFSET,
    GPU_POOL_CLAIM_TABLE_ROW_PUBLISHED_GENERATION_OFFSET, GPU_POOL_CLAIM_TABLE_ROWS,
    GpuPoolClaimTableRow, GpuPoolClaimantColumn, GpuPoolFrameRecycled, GpuPoolSlotRehandTest,
};
#[cfg(all(target_os = "macos", not(loom)))]
pub use gpu_pool_claim_table_mach_memory_entry::GpuPoolClaimTableMachMemoryEntry;
#[cfg(all(target_os = "linux", not(loom)))]
pub use gpu_pool_claim_table_memfd::GpuPoolClaimTableMemfd;
pub use gpu_pool_slot_frame_descriptor_body::{
    GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET, GPU_POOL_SLOT_FRAME_BODY_IMAGE_LAYOUT_OFFSET,
    GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET, GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
    GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET, GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
    GpuPoolSlotFrameDescriptorBody,
};
pub use helper_process_loss_count_board::{
    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_A_STANDALONE_NODE,
    FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED,
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessLossCountBoardKey,
    HelperProcessStepProgressBoardEntry, InboundLinkLossCountBoardSlot,
    OutputPortBagCountBoardEntry, STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS,
};
pub use payload_descriptor_bodies::{
    ARRAY_FRAME_ATTRIBUTE_DTYPE_BYTES, ARRAY_FRAME_ATTRIBUTE_MAX_DIMENSIONS,
    ARRAY_FRAME_ATTRIBUTE_NAME_BYTES, ARRAY_FRAME_ATTRIBUTE_ROW_BYTES,
    ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET,
    ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_OFFSET_OFFSET,
    ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET, ARRAY_FRAME_ATTRIBUTE_ROW_DTYPE_OFFSET,
    ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET, ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET,
    ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET, ARRAY_FRAME_CPU_DATA_ALIGNMENT_BYTES,
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameAttributeTableRefusal,
    ArrayFrameCpuAttributeToPack, ArrayFrameDescriptorBody,
    CHANNELS_BLOCK_BODY_CHANNEL_COUNT_OFFSET, CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET,
    CHANNELS_BLOCK_BODY_SAMPLE_TYPE_OFFSET, CHANNELS_BLOCK_BODY_SAMPLES_PER_CHANNEL_OFFSET,
    CHANNELS_BLOCK_BODY_START_SAMPLE_OFFSET, CHANNELS_BLOCK_BODY_USED_BYTES,
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, IMAGE_FRAME_BODY_COLOUR_OFFSET,
    IMAGE_FRAME_BODY_FORMAT_OFFSET, IMAGE_FRAME_BODY_HEIGHT_OFFSET, IMAGE_FRAME_BODY_USED_BYTES,
    IMAGE_FRAME_BODY_WIDTH_OFFSET, ImageFrameDescriptorBody, ImageFrameH273Colour,
    PACKETS_UNIT_BODY_CODEC_BYTES, PacketsUnitCodecNameDoesNotFit, PacketsUnitDescriptorBody,
    RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES, array_frame_attribute_table_of,
    array_frame_cpu_data_offsets, array_frame_payload_of_cpu_attributes,
};
pub use port_key::{MAX_PORT_KEY_SIZE, PortKey, PortKeyError};
pub use reserved_slot_reader::{
    LossTallyOfAReservedSlotReader, MessageReadFromAReservedSlot, OUTPUT_PORT_VIEWER_RING_DEPTH,
    OutputPortViewerReader, OutputPortViewerSubscribeRefusal, OutputPortViewerWake,
    SAMPLES_ONE_VIEWER_READ_TAKES_AT_MOST,
};
pub use stream_clock_board::{
    STREAM_CLOCK_BOARD_MODE_REAL, STREAM_CLOCK_BOARD_MODE_SCALED, STREAM_CLOCK_BOARD_MODE_STEPPED,
    StreamClockBoardEntry, StreamClockBoardKey,
};
pub use streamlib_runtime_client_contract::local_api_wire_contract::OutputPortViewerReads;

/// A claim table's rows in memory every interpreter handed the table maps:
/// a sealed memfd on Linux.
#[cfg(all(target_os = "linux", not(loom)))]
pub type GpuPoolClaimTableSharedRows = GpuPoolClaimTableMemfd;
/// A claim table's rows in memory every interpreter handed the table maps:
/// a Mach memory entry on macOS.
#[cfg(all(target_os = "macos", not(loom)))]
pub type GpuPoolClaimTableSharedRows = GpuPoolClaimTableMachMemoryEntry;
