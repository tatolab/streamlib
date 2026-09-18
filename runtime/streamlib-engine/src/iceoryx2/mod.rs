// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod audio_window;
mod channel_ceiling;
mod channel_name;
#[cfg(test)]
mod channel_sizing_tests;
mod delivery_profile;
mod helper_process_loss_count_board;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod iceoryx2_domain_for_this_test_process;
mod input;
mod loss_counters;
mod mailbox;
mod node;
mod output;
mod payload;
mod posix_shared_memory_headroom;
mod read_mode;

#[cfg(test)]
pub(crate) use audio_window::{AudioBlockSampleDtype, encode_an_audio_block_onto_the_wire};
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
pub use channel_name::{
    CHANNEL_CHUNK_SEPARATOR, ChannelName, InboundLinkName, MAX_CHANNEL_NAME_BYTES,
    describe_the_one_chunk_grammar, first_reason_this_is_not_one_channel_name_chunk,
    source_channel_name, validate_channel_name,
};
pub(crate) use delivery_profile::delivery_profile_for_input_port;
pub use delivery_profile::{DeliveryProfile, DeliveryResolution};
#[cfg(test)]
pub(crate) use helper_process_loss_count_board::a_loss_count_board_and_its_helpers_writer_for_this_test_process;
pub use helper_process_loss_count_board::{
    HelperPlacedProcessorLossCounts, HelperProcessLossCountBoard,
    HelperProcessLossCountBoardWriter, InboundLinkLossCountBoardSlotAndWiringGeneration,
    InboundLinkLossCountBoardSlotMirror, OutputPortRefusedBagCountBoardMirror,
    ProcessorLossCountSnapshot,
};
#[cfg(any(test, feature = "test-support"))]
pub use iceoryx2_domain_for_this_test_process::{
    Iceoryx2DomainForThisTestProcess, create_iceoryx2_node_for_this_test_process,
    iceoryx2_domain_for_this_test_process,
};
pub use input::{BoundedReadOutcome, InputMailboxes, InputMailboxesInner};
pub use loss_counters::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink, RefusedBagCountsByOutputPort,
};
pub use mailbox::{
    PortMailbox, PortMailboxDeliveredBag, PortMailboxEvictionNotice, PortMailboxQueuedFrameMeasure,
};
pub(crate) use node::ChannelSizing;
pub use node::{
    ChannelDataServicePublisher, ChannelDataServiceSubscriber, ChannelTapSubscribeError,
    ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES, ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
    Iceoryx2Node, Iceoryx2NotifyService, Iceoryx2Service,
    create_iceoryx2_node_in_engine_owned_domain, engine_owned_iceoryx2_config,
    engine_owned_iceoryx2_prefix_for_this_user, reclaim_dead_iceoryx2_nodes_in_engine_owned_domain,
};
pub use output::{ChannelEgressConfig, OutputWriter, OutputWriterInner};
pub use payload::{
    ChannelTrustTier, DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelBagSequenceNumberUserHeader,
    FRAME_HEADER_PAYLOAD_LEN_SIZE, FRAME_HEADER_SIZE, FRAME_HEADER_TIMESTAMP_NS_SIZE, FrameHeader,
    MAX_PORT_KEY_SIZE, MAX_PUBLISHERS_PER_CHANNEL, PortKey,
    RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL, TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
};
pub use posix_shared_memory_headroom::warn_when_posix_shared_memory_is_short_for_a_runtime;
pub use read_mode::ReadMode;
