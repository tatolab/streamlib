// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod input;
mod mailbox;
mod node;
mod output;
mod payload;
mod read_mode;

pub use input::InputMailboxes;
pub use mailbox::PortMailbox;
pub use node::{Iceoryx2EventService, Iceoryx2Node, Iceoryx2NotifyService, Iceoryx2Service};
pub use output::OutputWriter;
pub use payload::{
    EventPayload, FrameHeader, FramePayload, PortKey, SchemaName, TopicKey, FRAME_HEADER_SIZE,
    MAX_EVENT_PAYLOAD_SIZE, MAX_PAYLOAD_SIZE, MAX_SCHEMA_NAME_SIZE, MAX_TOPIC_KEY_SIZE,
};
pub use read_mode::ReadMode;
