// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod audio_window;
mod channel_ceiling;
mod channel_idle_poll_backoff;
mod channel_name;
#[cfg(test)]
pub(crate) mod channel_sample_test_support;
#[cfg(test)]
mod channel_sizing_tests;
mod child_process_start_outside_listener_binds;
mod delivery_profile;
mod helper_process_loss_count_board;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod iceoryx2_domain_for_this_test_process;
mod input;
mod input_port_borrow_quota;
mod input_port_message_payload;
mod loss_counters;
mod mailbox;
mod message_group_loss_recovery_gate;
mod node;
mod output;
mod payload;
mod posix_shared_memory_headroom;
mod read_mode;
mod stream_clock_board;

#[cfg(test)]
pub(crate) use audio_window::channels_block_of_one_window;
pub use audio_window::{
    AudioWindowContractMatchingADeviceStream, DeviceMatchedAudioWindowContractsByInputPort,
    ResolvedAudioWindowContract,
};
pub(crate) use audio_window::{
    AudioWindowDeclarationOfAnInputPort, WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
    audio_windowing_declared_by_input_port, refuse_an_unsettled_match_device_sentinel,
};
pub use channel_ceiling::{
    ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED, ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION,
    effective_channel_chunk_ceiling_bytes,
};
pub use channel_idle_poll_backoff::{
    CHANNEL_LONGEST_IDLE_POLL_BACKOFF, CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS,
    CHANNEL_SHORTEST_IDLE_POLL_BACKOFF, ChannelIdlePollBackoff,
};
pub use channel_name::{
    CHANNEL_CHUNK_SEPARATOR, ChannelName, InboundLinkName, MAX_CHANNEL_NAME_BYTES,
    source_channel_name, validate_channel_name,
};
pub(crate) use child_process_start_outside_listener_binds::bind_an_iceoryx2_listener_outside_every_child_process_start;
pub use child_process_start_outside_listener_binds::spawn_outside_every_iceoryx2_listener_bind;
pub(crate) use delivery_profile::delivery_profile_for_input_port;
pub use delivery_profile::{DeliveryProfile, DeliveryResolution};
#[cfg(test)]
pub(crate) use helper_process_loss_count_board::a_loss_count_board_and_its_helpers_writer_for_this_test_process;
pub use helper_process_loss_count_board::{
    HelperPlacedProcessorLossCounts, HelperProcessLossCountBoard,
    HelperProcessLossCountBoardWriter, InboundLinkLossCountBoardSlotAndWiringGeneration,
    InboundLinkLossCountBoardSlotMirror, OutputPortBagCountBoardMirror, ProcessorLossCountSnapshot,
};
#[cfg(test)]
pub(crate) use iceoryx2_domain_for_this_test_process::a_notifier_nothing_listens_on;
#[cfg(any(test, feature = "test-support"))]
pub use iceoryx2_domain_for_this_test_process::{
    Iceoryx2DomainForThisTestProcess, create_iceoryx2_node_for_this_test_process,
    iceoryx2_domain_for_this_test_process,
};
pub use input::{InputMailboxes, InputMailboxesInner, MessageReadFromInputPort};
pub use input_port_borrow_quota::{
    DEFAULT_INPUT_PORT_BORROW_QUOTA, MAX_INPUT_PORT_BORROW_QUOTA, input_port_borrow_quota,
};
pub use input_port_message_payload::{
    BorrowedChannelSample, InputPortMessagePayload, SamplesOneInboundLinkHolds,
};
pub use loss_counters::{
    BagCountsByOutputPort, DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
};
pub use mailbox::{
    PortMailbox, PortMailboxDeliveredBag, PortMailboxEvictionNotice, PortMailboxQueuedFrameMeasure,
};
pub use message_group_loss_recovery_gate::{
    MessageGroupLossRecoveryDisposition, MessageGroupLossRecoveryGate,
};
pub use payload::MessageGroupBoundary;

pub(crate) use node::ChannelSizing;
pub use node::{
    ChannelDataServiceLoanedSample, ChannelDataServicePublisher, ChannelDataServiceSample,
    ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError,
    ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES, ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
    Iceoryx2Node, Iceoryx2NotifyService, Iceoryx2Service,
    create_iceoryx2_node_in_engine_owned_domain, engine_owned_iceoryx2_config,
    engine_owned_iceoryx2_prefix_for_this_user, reclaim_dead_iceoryx2_nodes_in_engine_owned_domain,
};
pub use output::{ChannelEgressConfig, OutputPortPayloadLoan, OutputWriter, OutputWriterInner};
pub use payload::{
    ARRAY_FRAME_ATTRIBUTE_ROW_BYTES, ArrayFrameAttributeResidency, ArrayFrameAttributeRow,
    ArrayFrameAttributeTableRefusal, ArrayFrameCpuAttributeToPack, ArrayFrameDescriptorBody,
    ChannelTrustTier, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES, DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelPayloadCacheLine, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, DataChannelSamplePayloadRefusal, DataChannelSampleUserHeader,
    GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
    MAX_PORT_KEY_SIZE, MAX_PUBLISHERS_PER_CHANNEL, PacketsUnitCodecNameDoesNotFit,
    PacketsUnitDescriptorBody, PortKey, RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES,
    RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL, TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES, array_frame_attribute_table_of,
    array_frame_cpu_data_offsets, array_frame_payload_of_cpu_attributes,
    data_channel_payload_bytes_of, data_channel_payload_cache_lines_for,
};
pub use posix_shared_memory_headroom::warn_when_posix_shared_memory_is_short_for_a_runtime;
pub use read_mode::ReadMode;
pub use stream_clock_board::{
    StreamClockBoard, StreamClockBoardPublishListener, StreamClockBoardReader,
};
