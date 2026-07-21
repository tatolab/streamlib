// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod channel_ceiling;
mod delivery_profile;
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
pub use delivery_profile::{DeliveryProfile, DeliveryResolution, FlowClass};
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
    SCHEMA_IDENT_WIRE_MAX_ORG_LEN, SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN, SCHEMA_IDENT_WIRE_MAX_TYPE_LEN,
    SCHEMA_IDENT_WIRE_SIZE, SchemaIdentWire, SchemaIdentWireError,
    TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES, TopicKey,
    UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
};
pub use read_mode::ReadMode;
