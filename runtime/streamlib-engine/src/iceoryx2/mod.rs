// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod channel_ceiling;
mod channel_name;
#[cfg(test)]
mod channel_sizing_tests;
mod delivery_profile;
mod dropped_bag_counters;
mod input;
mod mailbox;
mod node;
mod output;
mod overflow;
mod payload;
mod read_mode;

pub use channel_ceiling::{
    ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED, ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION,
    effective_channel_ceiling_bytes,
};
pub use channel_name::{
    CHANNEL_CHUNK_SEPARATOR, ChannelName, MAX_CHANNEL_NAME_BYTES, source_channel_name,
    validate_channel_name,
};
pub(crate) use delivery_profile::delivery_profile_for_input_port;
pub use delivery_profile::{DeliveryProfile, DeliveryResolution};
pub use dropped_bag_counters::{DroppedBagCountsByInboundLink, InboundLinkDroppedBagCounter};
pub use input::{BoundedReadOutcome, InputMailboxes, InputMailboxesInner};
pub use mailbox::PortMailbox;
pub use node::{
    ChannelTapSubscribeError, Iceoryx2EventService, Iceoryx2Node, Iceoryx2NotifyService,
    Iceoryx2Service,
};
pub use output::{ChannelEgressConfig, OutputWriter, OutputWriterInner};
pub use overflow::Overflow;
pub use payload::{
    ChannelTrustTier, DEFAULT_EXPECTED_PAYLOAD_BYTES, DEFAULT_MAX_QUEUED_MESSAGES, EventPayload,
    FRAME_HEADER_SIZE, FrameHeader, MAX_EVENT_PAYLOAD_SIZE, MAX_PUBLISHERS_PER_CHANNEL,
    MAX_TOPIC_KEY_SIZE, PortKey, RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
    TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES, TopicKey,
    UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
};
pub use read_mode::ReadMode;
