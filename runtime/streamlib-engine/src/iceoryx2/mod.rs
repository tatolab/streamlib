// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod input;
mod mailbox;
mod node;
mod output;
mod overflow;
mod payload;
mod read_mode;

pub use input::{InputMailboxes, InputMailboxesInner};
pub use mailbox::PortMailbox;
pub use node::{Iceoryx2EventService, Iceoryx2Node, Iceoryx2NotifyService, Iceoryx2Service};
pub use output::{OutputWriter, OutputWriterInner};
pub use overflow::Overflow;
pub use payload::{
    DEFAULT_MAX_QUEUED_MESSAGES, EventPayload, FRAME_HEADER_SIZE, FrameHeader, FramePayload,
    MAX_EVENT_PAYLOAD_SIZE, MAX_FANIN_PER_DESTINATION, MAX_PAYLOAD_SIZE,
    MAX_SUBSCRIBERS_PER_DESTINATION, MAX_TOPIC_KEY_SIZE, PortKey, SCHEMA_IDENT_WIRE_MAX_ORG_LEN,
    SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN, SCHEMA_IDENT_WIRE_MAX_TYPE_LEN, SCHEMA_IDENT_WIRE_SIZE,
    SchemaIdentWire, SchemaIdentWireError, TopicKey,
};
pub use read_mode::ReadMode;
